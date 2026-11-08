//! Starting things from the phone (issue #1534): a direct conversation, a channel, a space, and the
//! people picker behind them. Desktop and web run these through the UI adapter's command queue;
//! the apps await them instead, so a sheet closes only once the server took the change and the new
//! conversation or space can open right away. Server refusals come back as outcomes the sheet shows
//! at its field (forms.md); a lost connection is an error, and a retry with the same `request_key`
//! creates nothing twice.

use std::collections::HashSet;

use teamcraft_client_api::backend::ChatBackend as _;
use teamcraft_client_api::model::{ConversationId, SpaceId, User, UserId};
use teamcraft_client_api::remote::ChatAnswer;
use teamcraft_core::CoreError;
use teamcraft_core::api::ApiError;
use teamcraft_core::api::ServerApi as _;
use teamcraft_core::client::workspace_admin::ChannelOptions;
use teamcraft_core::ui_backend::CoreCommand;
use teamcraft_proto::role_rules::can_in_workspace;
use teamcraft_proto::{
  CreateSpaceRequest, ErrorCode, HistoryPolicy, IdempotencyKey, JoinPolicy, Permission, PermissionScheme,
};

use crate::TeamCraft;
use crate::bridge::to_json;
use crate::engine::{parse_id, parse_ids};
use crate::error::TeamCraftError;
use crate::reads::Reads;

/// The most people one picker search answers with; the list on the phone shows that many rows at
/// most, the user narrows by typing.
const MAX_PEOPLE_RESULTS: usize = 100;

/// How a create request ended when the server answered.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CreateOutcome {
  /// Created (or found, for a retry or an existing direct conversation): the new conversation's
  /// or space's ID.
  Created { id: String },
  /// The server refused the name (empty, too long, control characters): show it at the name
  /// field.
  NameRefused { detail: String },
  /// The user may not create this here (any more): the role changed since the menu was shown.
  NotAllowed,
}

/// How adding people to a channel ended when the server answered.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AddPeopleOutcome {
  /// Everybody is in the channel now.
  Added,
  /// Somebody isn't in the channel's space, and the user may not add them to it: a space admin
  /// invites them first.
  NotInSpace,
  /// The user may not add people here (any more).
  NotAllowed,
}

/// Whom "Add people" puts into a channel, as the core decides it.
struct ChannelAddition {
  people: Vec<(UserId, bool)>,
  joins_space: bool,
}

/// What the "new channel" sheet asks for.
#[derive(Clone, Debug, uniffi::Record)]
pub struct NewChannelInput {
  pub space_id: String,
  pub name: String,
  pub is_private: bool,
  /// Shown under the channel's name in lists ("topic" on the wire); empty for none.
  pub description: String,
  /// From [`crate::new_request_key`], once per attempt the user starts; kept for retries.
  pub request_key: String,
}

/// What the "new space" sheet asks for.
#[derive(Clone, Debug, uniffi::Record)]
pub struct NewSpaceInput {
  pub name: String,
  /// Empty for none.
  pub description: String,
  /// Anyone in the workspace may join; otherwise by invitation.
  pub is_open: bool,
  pub request_key: String,
}

#[uniffi::export]
impl TeamCraft {
  /// People whose name or username matches `query`, as the `people` answer of the `search_people`
  /// query (JSON), at most `limit` (capped at 100), without the user, bots and deactivated
  /// accounts. The people the device knows answer at once; the
  /// server's directory is asked once per query and a while, and its answer arrives as a change,
  /// so read again then. Unlike `query`, this hands that directory request to the client.
  pub async fn search_people(&self, query: String, limit: u32) -> Result<String, TeamCraftError> {
    let (answer, commands) = self.read(move |reads| reads.search_people(&query, limit)).await?;
    if !commands.is_empty() {
      self
        .call(move |engine| Box::pin(engine.run_commands(commands)))
        .await?;
    }
    Ok(answer)
  }

  /// Whether the user may create spaces (`create_space`), so "New space" shows only to them. As on
  /// desktop: the instance role from the server and the built-in scheme (admins and owners); the
  /// server still decides, and a refusal comes back as [`CreateOutcome::NotAllowed`].
  pub async fn can_create_spaces(&self) -> Result<bool, TeamCraftError> {
    self
      .call(|engine| {
        Box::pin(async move {
          let me = engine.api().me().await.map_err(CoreError::from)?;
          Ok(can_in_workspace(
            &PermissionScheme::default(),
            &me.role,
            &Permission::CreateSpace,
          ))
        })
      })
      .await
  }

  /// Opens, or finds, the direct conversation with exactly these people and the user: a DM for
  /// one, a group DM for several.
  pub async fn open_direct(&self, user_ids: Vec<String>) -> Result<CreateOutcome, TeamCraftError> {
    let users: Vec<UserId> = parse_ids(&user_ids)?;
    let [first, ..] = users.as_slice() else {
      return Err(TeamCraftError::InvalidInput("nobody to message".into()));
    };
    let first = *first;
    let is_one = users.len() == 1;
    let result = self
      .call(move |engine| {
        Box::pin(engine.with_client(move |client| {
          Box::pin(async move {
            refusing(if is_one {
              client.open_dm(first).await
            } else {
              client.open_group_dm(&users).await
            })
          })
        }))
      })
      .await;
    created(result, |id: ConversationId| id.to_string())
  }

  /// Adds people to a channel, as desktop does: first to the channel's space where the user may
  /// add or invite people there (guest accounts as guests), then to the channel's encryption group
  /// with the history from their join on. People already in it are left alone.
  pub async fn add_people_to_channel(
    &self,
    conversation_id: String,
    user_ids: Vec<String>,
  ) -> Result<AddPeopleOutcome, TeamCraftError> {
    let conversation: ConversationId = parse_id(&conversation_id)?;
    let users: Vec<UserId> = parse_ids(&user_ids)?;
    let addition = self
      .read(move |reads| reads.channel_addition(conversation, &users))
      .await?;
    let result = self
      .call(move |engine| {
        Box::pin(engine.with_client(move |client| {
          Box::pin(async move {
            let added = client
              .add_people_to_channel(
                conversation,
                &addition.people,
                HistoryPolicy::FromJoin,
                addition.joins_space,
              )
              .await;
            match added {
              Ok(()) => Ok(AddPeopleOutcome::Added),
              Err(error) => addition_refusal(&error).ok_or(error),
            }
          })
        }))
      })
      .await?;
    Ok(result)
  }

  /// Creates a channel in a space, with its encryption group, and answers its ID.
  pub async fn create_channel(&self, input: NewChannelInput) -> Result<CreateOutcome, TeamCraftError> {
    let space: SpaceId = parse_id(&input.space_id)?;
    let options = ChannelOptions {
      name: input.name,
      is_private: input.is_private,
      maybe_topic: non_empty(input.description),
      maybe_category: None,
      maybe_history: None,
      is_integration: false,
      maybe_idempotency_key: Some(request_key(&input.request_key)?),
    };
    let result = self
      .call(move |engine| {
        Box::pin(engine.with_client(move |client| {
          Box::pin(async move { refusing(client.create_channel_with(space, options).await) })
        }))
      })
      .await;
    created(result, |id: ConversationId| id.to_string())
  }

  /// Creates a space, with the user as its admin, and answers its ID.
  pub async fn create_space(&self, input: NewSpaceInput) -> Result<CreateOutcome, TeamCraftError> {
    let request = CreateSpaceRequest {
      name: input.name,
      maybe_icon: None,
      maybe_color: None,
      maybe_description: non_empty(input.description),
      join_policy: if input.is_open {
        JoinPolicy::Open
      } else {
        JoinPolicy::InviteOnly
      },
      maybe_idempotency_key: Some(request_key(&input.request_key)?),
    };
    let result = self
      .call(move |engine| {
        Box::pin(engine.with_client(move |client| {
          Box::pin(async move { refusing(client.create_space(&request).await.map(|space| space.id)) })
        }))
      })
      .await;
    created(result, |id: SpaceId| id.to_string())
  }
}

impl Reads {
  /// The `people` answer and the directory request the search queued, if it is due.
  fn search_people(&self, query: &str, limit: u32) -> Result<(String, Vec<CoreCommand>), TeamCraftError> {
    let backend = self.backend()?;
    let me = backend.me().id;
    let max = usize::try_from(limit)
      .unwrap_or(usize::MAX)
      .min(MAX_PEOPLE_RESULTS);
    let people: Vec<User> = backend
      .search_people(query)
      .into_iter()
      // As on desktop: never the user, bots or deactivated accounts (issue #654).
      .filter(|user| user.id != me && !user.is_bot && !user.is_deactivated && !user.is_deleted)
      .take(max)
      .collect();
    let commands = backend.drain_commands();
    Ok((to_json(&ChatAnswer::People { people })?, commands))
  }

  /// The people to add (who of them is a guest account) and whether they join the space first.
  fn channel_addition(
    &self,
    conversation: ConversationId,
    users: &[UserId],
  ) -> Result<ChannelAddition, TeamCraftError> {
    let backend = self.backend()?;
    let maybe_conversation = backend.conversation(conversation);
    let members: HashSet<UserId> = maybe_conversation
      .as_ref()
      .map(|found| found.member_ids.iter().copied().collect())
      .unwrap_or_default();
    let joins_space = maybe_conversation
      .and_then(|found| found.maybe_space)
      .and_then(|space| {
        backend
          .spaces()
          .into_iter()
          .find(|candidate| candidate.id == space)
      })
      .is_some_and(|space| space.can_add_members || space.can_invite_guests);
    let people = users
      .iter()
      .filter(|user| !members.contains(user))
      .map(|user| (*user, backend.user(*user).is_some_and(|found| found.is_guest)))
      .collect();
    Ok(ChannelAddition { people, joins_space })
  }
}

/// A refusal of adding people the sheet explains, if it is one.
fn addition_refusal(error: &CoreError) -> Option<AddPeopleOutcome> {
  let CoreError::Api(api @ ApiError::Status { status, .. }) = error else {
    return None;
  };
  match (api.error_code(), *status) {
    (Some(ErrorCode::NotSpaceMember), _) => Some(AddPeopleOutcome::NotInSpace),
    (Some(ErrorCode::Forbidden), _) | (_, 403) => Some(AddPeopleOutcome::NotAllowed),
    _ => None,
  }
}

/// The outcome of a create call: the server's refusals the sheet shows, other errors as errors.
fn created<T>(
  result: Result<Result<T, CreateOutcome>, TeamCraftError>,
  id_text: impl FnOnce(T) -> String,
) -> Result<CreateOutcome, TeamCraftError> {
  Ok(match result? {
    Ok(id) => CreateOutcome::Created { id: id_text(id) },
    Err(refusal) => refusal,
  })
}

/// Sets a refusal the sheet shows apart from the errors that stay errors (network, storage).
fn refusing<T>(result: Result<T, CoreError>) -> Result<Result<T, CreateOutcome>, CoreError> {
  match result {
    Ok(value) => Ok(Ok(value)),
    Err(error) => create_refusal(&error).map(Err).ok_or(error),
  }
}

/// A refusal of a create request the sheet shows at a field or as "not allowed", if it is one.
fn create_refusal(error: &CoreError) -> Option<CreateOutcome> {
  let CoreError::Api(api @ ApiError::Status { status, .. }) = error else {
    return None;
  };
  match (api.error_code(), *status) {
    (Some(ErrorCode::InvalidRequest), _) => Some(CreateOutcome::NameRefused {
      detail: api.to_string(),
    }),
    (Some(ErrorCode::Forbidden), _) | (_, 403) => Some(CreateOutcome::NotAllowed),
    _ => None,
  }
}

fn request_key(text: &str) -> Result<IdempotencyKey, TeamCraftError> {
  IdempotencyKey::from_hex(text).map_err(|_| TeamCraftError::InvalidInput("not a request key".into()))
}

fn non_empty(text: String) -> Option<String> {
  let trimmed = text.trim();
  (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::ErrorBody;

  use super::*;

  #[test]
  fn invalid_names_go_to_the_name_field() {
    assert!(matches!(
      create_refusal(&refused(400, Some(ErrorCode::InvalidRequest))),
      Some(CreateOutcome::NameRefused { .. })
    ));
  }

  #[test]
  fn forbidden_is_not_allowed_with_or_without_a_body() {
    assert_eq!(
      create_refusal(&refused(403, Some(ErrorCode::Forbidden))),
      Some(CreateOutcome::NotAllowed)
    );
    assert_eq!(
      create_refusal(&refused(403, None)),
      Some(CreateOutcome::NotAllowed)
    );
  }

  #[test]
  fn other_failures_stay_errors() {
    assert_eq!(create_refusal(&refused(503, None)), None);
    assert_eq!(
      create_refusal(&refused(404, Some(ErrorCode::SpaceNotFound))),
      None
    );
    assert_eq!(
      create_refusal(&CoreError::Api(ApiError::Network("offline".into()))),
      None
    );
    let kept = refusing::<()>(Err(CoreError::Api(ApiError::Network("offline".into()))));
    assert!(matches!(kept, Err(CoreError::Api(ApiError::Network(_)))));
  }

  #[test]
  fn adding_people_outside_the_space_is_said_so() {
    assert_eq!(
      addition_refusal(&refused(400, Some(ErrorCode::NotSpaceMember))),
      Some(AddPeopleOutcome::NotInSpace)
    );
    assert_eq!(
      addition_refusal(&refused(403, None)),
      Some(AddPeopleOutcome::NotAllowed)
    );
    assert_eq!(addition_refusal(&refused(500, None)), None);
  }

  #[test]
  fn request_keys_must_be_the_apps_keys() {
    assert!(request_key(&crate::new_request_key()).is_ok());
    assert!(matches!(
      request_key("not hex"),
      Err(TeamCraftError::InvalidInput(_))
    ));
  }

  #[test]
  fn blank_descriptions_are_none() {
    assert_eq!(non_empty("  ".into()), None);
    assert_eq!(non_empty(" Plans ".into()), Some("Plans".into()));
  }

  fn refused(status: u16, maybe_code: Option<ErrorCode>) -> CoreError {
    CoreError::Api(ApiError::Status {
      status,
      maybe_body: maybe_code.map(|error_code| ErrorBody::new(error_code, "refused")),
      maybe_retry_after: None,
    })
  }
}
