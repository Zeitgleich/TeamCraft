//! The directory on the phones (issue #1337): people with search and profile filters, external
//! people, channels across the user's spaces and the open invitations the user may manage, each a
//! page at a time from the routes of #1334, and "Invite people". The server decides what each list
//! holds and what the user may do; the apps show the buttons its answers allow.
//!
//! The requests run on the HTTP runtime, not on the engine thread: browsing the directory never
//! holds up syncing, and syncing never holds up a page.

use std::future::Future;

use teamcraft_core::CoreError;
use teamcraft_core::api::{ApiError, ServerApi as _};
use teamcraft_proto::{
  ConversationVisibility, CreateInviteRequest, DirectoryChannel, DirectoryChannelsQuery,
  DirectoryFiltersQuery, DirectoryInvitation, DirectoryInvitationsQuery, DirectoryPeopleQuery,
  DirectoryPerson, ErrorCode, Id, InviteId, SpaceId, SpaceRole, UserKind,
};

use crate::TeamCraft;
use crate::engine::{Api, parse_id};
use crate::error::TeamCraftError;
use crate::http_fetch::run_request;

/// Which people a directory list shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum DirectoryPeopleKind {
  /// Everybody the user may see: a guest's own list, and a space's roster.
  Everybody,
  /// The People tab: the workspace's members.
  Members,
  /// The External tab: guests.
  Guests,
}

/// What a people list asks for. Empty strings are "no filter".
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DirectoryPeopleRequest {
  pub query: String,
  pub kind: DirectoryPeopleKind,
  pub organization: String,
  pub department: String,
  pub title: String,
  /// Only members of this space; empty for all the user's spaces.
  pub space_id: String,
  /// `next_after` of the previous page; empty for the first.
  pub after: String,
}

/// One person of the directory.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DirectoryPersonItem {
  pub user_id: String,
  pub display_name: String,
  /// Empty when the server doesn't show it.
  pub username: String,
  pub is_guest: bool,
  pub organization: String,
  pub title: String,
  pub department: String,
  /// `online`, `away`, `dnd` or `offline`, as of this answer.
  pub presence: String,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DirectoryPeoplePage {
  pub people: Vec<DirectoryPersonItem>,
  /// Where the next page starts; empty on the last page.
  pub next_after: String,
}

/// The values the people filters offer.
#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct DirectoryFilterChoices {
  pub organizations: Vec<String>,
  pub departments: Vec<String>,
  pub titles: Vec<String>,
}

/// A channel of one of the user's spaces: one they are in, or a public one.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DirectoryChannelItem {
  pub conversation_id: String,
  pub space_id: String,
  pub space_name: String,
  pub name: String,
  pub topic: String,
  pub is_private: bool,
  pub member_count: u32,
  /// As of this answer: "Open", else "Join".
  pub is_member: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DirectoryChannelsPage {
  pub channels: Vec<DirectoryChannelItem>,
  pub next_after: String,
}

/// An open invitation the user may manage, newest first.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DirectoryInvitationItem {
  pub invitation_id: String,
  pub space_id: String,
  pub space_name: String,
  pub email: String,
  pub is_guest: bool,
  pub sponsor_name: String,
  /// Unix milliseconds; `None` for an invitation without expiry.
  pub maybe_expires_at: Option<i64>,
  pub is_expired: bool,
  /// The server can send the mail again now; the resend still may be refused as too soon.
  pub can_resend: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DirectoryInvitationsPage {
  pub invitations: Vec<DirectoryInvitationItem>,
  pub next_after: String,
}

/// What the "Invite people" sheet asks for.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct InvitePeopleInput {
  pub space_id: String,
  pub email: String,
  /// As a guest (external); otherwise as a member.
  pub is_guest: bool,
  /// A note for the e-mail; empty for none.
  pub message: String,
}

/// How an invitation ended when the server answered.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum InviteOutcome {
  Sent,
  /// The server refused the address (malformed, or a domain guests may not come from): shown at
  /// the e-mail field.
  EmailRefused {
    detail: String,
  },
  /// This address already has an open invitation to the space: shown at the e-mail field.
  AlreadyInvited,
  /// The user may not invite (in this role) here any more.
  NotAllowed,
}

/// How resending or revoking an invitation ended when the server answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum InvitationOutcome {
  Done,
  /// Mail went out to this address a short while ago, or the user's mail budget is used up: try
  /// later. Said at the row.
  TooSoon,
  /// Accepted or revoked meanwhile: the row goes away.
  Gone,
  NotAllowed,
}

#[uniffi::export]
impl TeamCraft {
  /// One page of the directory's people, narrowed by search and filters.
  pub async fn directory_people(
    &self,
    request: DirectoryPeopleRequest,
  ) -> Result<DirectoryPeoplePage, TeamCraftError> {
    let query = people_query(request)?;
    let page = self
      .directory_request(move |api| async move { api.directory_people(&query).await })
      .await?;
    Ok(DirectoryPeoplePage {
      people: page.people.into_iter().map(person_item).collect(),
      next_after: page.maybe_next_after.unwrap_or_default(),
    })
  }

  /// The organizations, departments and titles the people filters offer, for `kind` and an
  /// optional space (empty for all).
  pub async fn directory_filters(
    &self,
    kind: DirectoryPeopleKind,
    space_id: String,
  ) -> Result<DirectoryFilterChoices, TeamCraftError> {
    let query = DirectoryFiltersQuery {
      maybe_kind: user_kind(kind),
      maybe_space_id: maybe_id(&space_id)?,
    };
    let choices = self
      .directory_request(move |api| async move { api.directory_filters(&query).await })
      .await?;
    Ok(DirectoryFilterChoices {
      organizations: choices.organizations,
      departments: choices.departments,
      titles: choices.titles,
    })
  }

  /// One page of channels across the user's spaces (or one space), matched by name and topic.
  pub async fn directory_channels(
    &self,
    query: String,
    space_id: String,
    after: String,
  ) -> Result<DirectoryChannelsPage, TeamCraftError> {
    let query = DirectoryChannelsQuery {
      query: query.trim().to_owned(),
      maybe_space_id: maybe_id(&space_id)?,
      maybe_after: non_empty(after),
    };
    let page = self
      .directory_request(move |api| async move { api.directory_channels(&query).await })
      .await?;
    Ok(DirectoryChannelsPage {
      channels: page.channels.into_iter().map(channel_item).collect(),
      next_after: page.maybe_next_after.unwrap_or_default(),
    })
  }

  /// One page of the open invitations the user may manage, matched by address.
  pub async fn directory_invitations(
    &self,
    query: String,
    space_id: String,
    after: String,
  ) -> Result<DirectoryInvitationsPage, TeamCraftError> {
    let query = DirectoryInvitationsQuery {
      query: query.trim().to_owned(),
      maybe_space_id: maybe_id(&space_id)?,
      maybe_after: maybe_id(&after)?,
    };
    let page = self
      .directory_request(move |api| async move { api.directory_invitations(&query).await })
      .await?;
    Ok(DirectoryInvitationsPage {
      invitations: page.invitations.into_iter().map(invitation_item).collect(),
      next_after: page
        .maybe_next_after
        .map(|invite| invite.to_string())
        .unwrap_or_default(),
    })
  }

  /// Invites somebody by e-mail to a space, as member or guest.
  pub async fn invite_people(&self, input: InvitePeopleInput) -> Result<InviteOutcome, TeamCraftError> {
    let space: SpaceId = parse_id(&input.space_id)?;
    let request = CreateInviteRequest {
      email: input.email.trim().to_owned(),
      role: if input.is_guest {
        SpaceRole::Guest
      } else {
        SpaceRole::Member
      },
      channel_ids: Vec::new(),
      maybe_expires_at: None,
      maybe_message: non_empty(input.message),
    };
    let answer = self
      .directory_request(move |api| async move { Ok(api.create_invite(space, &request).await) })
      .await?;
    match answer {
      Ok(_) => Ok(InviteOutcome::Sent),
      Err(error) => invite_refusal(&error).ok_or_else(|| CoreError::from(error).into()),
    }
  }

  /// Sends an invitation's e-mail again.
  pub async fn resend_invitation(
    &self,
    space_id: String,
    invitation_id: String,
  ) -> Result<InvitationOutcome, TeamCraftError> {
    let space: SpaceId = parse_id(&space_id)?;
    let invite: InviteId = parse_id(&invitation_id)?;
    let answer = self
      .directory_request(move |api| async move { Ok(api.resend_invite(space, invite).await) })
      .await?;
    invitation_outcome(answer)
  }

  /// Withdraws an invitation; one that is gone already counts as withdrawn.
  pub async fn revoke_invitation(
    &self,
    space_id: String,
    invitation_id: String,
  ) -> Result<InvitationOutcome, TeamCraftError> {
    let space: SpaceId = parse_id(&space_id)?;
    let invite: InviteId = parse_id(&invitation_id)?;
    let answer = self
      .directory_request(move |api| async move { Ok(api.delete_invite(space, invite).await) })
      .await?;
    match invitation_outcome(answer)? {
      InvitationOutcome::Gone => Ok(InvitationOutcome::Done),
      other => Ok(other),
    }
  }
}

impl TeamCraft {
  /// Takes the server API from the engine (no request), then runs `request` on the HTTP runtime.
  async fn directory_request<R, F, Fut>(&self, request: F) -> Result<R, TeamCraftError>
  where
    R: Send + 'static,
    F: FnOnce(Api) -> Fut + Send + 'static,
    Fut: Future<Output = Result<R, ApiError>> + Send + 'static,
  {
    let api = self
      .call(|engine| Box::pin(async move { Ok(engine.api()) }))
      .await?;
    run_request(request(api))
      .await
      .map_err(TeamCraftError::Network)?
      .map_err(|error| CoreError::from(error).into())
  }
}

fn people_query(request: DirectoryPeopleRequest) -> Result<DirectoryPeopleQuery, TeamCraftError> {
  Ok(DirectoryPeopleQuery {
    query: request.query.trim().to_owned(),
    maybe_kind: user_kind(request.kind),
    maybe_organization: non_empty(request.organization),
    maybe_department: non_empty(request.department),
    maybe_title: non_empty(request.title),
    maybe_space_id: maybe_id(&request.space_id)?,
    maybe_after: non_empty(request.after),
  })
}

fn user_kind(kind: DirectoryPeopleKind) -> Option<UserKind> {
  match kind {
    DirectoryPeopleKind::Everybody => None,
    DirectoryPeopleKind::Members => Some(UserKind::Member),
    DirectoryPeopleKind::Guests => Some(UserKind::Guest),
  }
}

fn person_item(person: DirectoryPerson) -> DirectoryPersonItem {
  DirectoryPersonItem {
    user_id: person.user.id.to_string(),
    display_name: person.user.display_name,
    username: person.user.maybe_username.unwrap_or_default(),
    is_guest: person.user.kind == UserKind::Guest,
    organization: person.user.maybe_organization.unwrap_or_default(),
    title: person.maybe_title.unwrap_or_default(),
    department: person.maybe_department.unwrap_or_default(),
    presence: person.presence.as_str().to_owned(),
  }
}

fn channel_item(channel: DirectoryChannel) -> DirectoryChannelItem {
  DirectoryChannelItem {
    conversation_id: channel.id.to_string(),
    space_id: channel.space_id.to_string(),
    space_name: channel.space_name,
    name: channel.name,
    topic: channel.maybe_topic.unwrap_or_default(),
    is_private: channel.visibility == ConversationVisibility::Private,
    member_count: channel.member_count,
    is_member: channel.is_member,
  }
}

fn invitation_item(invitation: DirectoryInvitation) -> DirectoryInvitationItem {
  DirectoryInvitationItem {
    invitation_id: invitation.id.to_string(),
    space_id: invitation.space_id.to_string(),
    space_name: invitation.space_name,
    email: invitation.email,
    is_guest: invitation.role == SpaceRole::Guest,
    sponsor_name: invitation.sponsor.display_name,
    maybe_expires_at: invitation.maybe_expires_at.map(|at| at.0),
    is_expired: invitation.is_expired,
    can_resend: invitation.can_resend,
  }
}

/// A refusal of an invitation the sheet shows, if it is one.
fn invite_refusal(error: &ApiError) -> Option<InviteOutcome> {
  let ApiError::Status { status, .. } = error else {
    return None;
  };
  match (error.error_code(), *status) {
    (Some(ErrorCode::InviteAlreadyOpen), _) => Some(InviteOutcome::AlreadyInvited),
    (Some(ErrorCode::InvalidRequest), _) => Some(InviteOutcome::EmailRefused {
      detail: error.to_string(),
    }),
    (Some(ErrorCode::Forbidden), _) | (_, 403) => Some(InviteOutcome::NotAllowed),
    _ => None,
  }
}

/// The outcome of resending or revoking; other failures stay errors.
fn invitation_outcome(answer: Result<(), ApiError>) -> Result<InvitationOutcome, TeamCraftError> {
  let error = match answer {
    Ok(()) => return Ok(InvitationOutcome::Done),
    Err(error) => error,
  };
  let ApiError::Status { status, .. } = &error else {
    return Err(CoreError::from(error).into());
  };
  match (error.error_code(), *status) {
    (Some(ErrorCode::RateLimited), _) | (_, 429) => Ok(InvitationOutcome::TooSoon),
    (Some(ErrorCode::InviteNotFound), _) => Ok(InvitationOutcome::Gone),
    (Some(ErrorCode::Forbidden), _) | (_, 403) => Ok(InvitationOutcome::NotAllowed),
    _ => Err(CoreError::from(error).into()),
  }
}

fn maybe_id<T: Id + std::str::FromStr>(text: &str) -> Result<Option<T>, TeamCraftError> {
  if text.trim().is_empty() {
    Ok(None)
  } else {
    parse_id(text).map(Some)
  }
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
  fn people_requests_leave_out_empty_filters() {
    let query = people_query(DirectoryPeopleRequest {
      query: " ann ".into(),
      kind: DirectoryPeopleKind::Guests,
      organization: String::new(),
      department: "Sales".into(),
      title: " ".into(),
      space_id: String::new(),
      after: String::new(),
    })
    .unwrap();
    assert_eq!(query.query, "ann");
    assert_eq!(query.maybe_kind, Some(UserKind::Guest));
    assert_eq!(query.maybe_organization, None);
    assert_eq!(query.maybe_department.as_deref(), Some("Sales"));
    assert_eq!(query.maybe_title, None);
    assert_eq!(query.maybe_space_id, None);
    assert_eq!(query.maybe_after, None);
  }

  #[test]
  fn everybody_asks_for_no_kind() {
    assert_eq!(user_kind(DirectoryPeopleKind::Everybody), None);
    assert_eq!(user_kind(DirectoryPeopleKind::Members), Some(UserKind::Member));
  }

  #[test]
  fn malformed_ids_are_refused_before_a_request() {
    assert!(matches!(
      maybe_id::<SpaceId>("not an id"),
      Err(TeamCraftError::InvalidInput(_))
    ));
    assert_eq!(maybe_id::<SpaceId>("").unwrap(), None);
  }

  mod refusals {
    use super::*;

    #[test]
    fn invitation_refusals_go_to_the_email_field_or_say_not_allowed() {
      assert_eq!(
        invite_refusal(&refused(409, Some(ErrorCode::InviteAlreadyOpen))),
        Some(InviteOutcome::AlreadyInvited)
      );
      assert!(matches!(
        invite_refusal(&refused(400, Some(ErrorCode::InvalidRequest))),
        Some(InviteOutcome::EmailRefused { .. })
      ));
      assert_eq!(
        invite_refusal(&refused(403, None)),
        Some(InviteOutcome::NotAllowed)
      );
      assert_eq!(invite_refusal(&refused(503, None)), None);
      assert_eq!(invite_refusal(&ApiError::Network("offline".into())), None);
    }

    #[test]
    fn resending_too_soon_and_gone_invitations_are_outcomes() {
      assert_eq!(
        invitation_outcome(Err(refused(429, Some(ErrorCode::RateLimited)))).unwrap(),
        InvitationOutcome::TooSoon
      );
      assert_eq!(
        invitation_outcome(Err(refused(404, Some(ErrorCode::InviteNotFound)))).unwrap(),
        InvitationOutcome::Gone
      );
      assert_eq!(
        invitation_outcome(Err(refused(403, None))).unwrap(),
        InvitationOutcome::NotAllowed
      );
      assert_eq!(invitation_outcome(Ok(())).unwrap(), InvitationOutcome::Done);
    }

    #[test]
    fn network_failures_stay_errors() {
      assert!(matches!(
        invitation_outcome(Err(ApiError::Network("offline".into()))),
        Err(TeamCraftError::Network(_))
      ));
      assert!(invitation_outcome(Err(refused(500, None))).is_err());
    }
  }

  fn refused(status: u16, maybe_code: Option<ErrorCode>) -> ApiError {
    ApiError::Status {
      status,
      maybe_body: maybe_code.map(|error_code| ErrorBody::new(error_code, "refused")),
      maybe_retry_after: None,
    }
  }
}
