use std::sync::Arc;

use serde_json::json;
use teamcraft_db::{DirectoryInvitationFilter, NewInvite, SpaceListing};
use teamcraft_proto::{
  ConversationKind, CreateInviteRequest, DirectoryInvitation, DirectoryInvitationsQuery,
  DirectoryInvitationsResponse, InviteId, InviteMail, InviteResponse, ListInvitesResponse, Permission,
  SpaceId, SpaceRole, Target, UserId,
};
use time::OffsetDateTime;
use tracing::warn;

use super::access::{allows, is_guest, is_instance_admin, load_space, space_target};
use super::audit::{self, Target as AuditTarget};
use super::convert::{invite_response, offset_date_time, presented_name, unix_millis, user_summary};
use super::directory::{check_search_text, page_size};
use super::spaces::require_member_management;
use crate::auth::AuthenticatedUser;
use crate::brand::Brand;
use crate::error::ApiError;
use crate::meetings::ical::Method;
use crate::meetings::mailer::OutgoingMail;
use crate::state::AppState;

const MAX_EMAIL_CHARS: usize = 254;
const MAX_INVITE_CHANNELS: usize = 100;

/// What a caller may do with the open invitations of one space. The one place that decides who sees,
/// resends and revokes them (docs/admin.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InviteReach {
  /// Every invitation of the space.
  manages_all: bool,
  /// Guest invitations the caller sent, while they may still invite guests here.
  has_own_guests: bool,
}

impl InviteReach {
  fn of(state: &AppState, caller: &AuthenticatedUser, target: &Target) -> Self {
    let is_allowed = |permission| !is_guest(caller) && allows(state, caller, &permission, target);
    Self {
      manages_all: is_allowed(Permission::ManageSpaceMembers),
      has_own_guests: is_allowed(Permission::InviteGuest),
    }
  }

  fn covers(self, caller: UserId, role: &SpaceRole, sponsor: UserId) -> bool {
    self.manages_all || (self.has_own_guests && *role == SpaceRole::Guest && sponsor == caller)
  }
}

/// Invites a person by email. The invite is applied when someone signs in with that verified
/// email address (accounts::authenticate); delivering an email to the invitee is not part of M1.
pub async fn create_invite(
  state: &AppState,
  caller: &crate::auth::AuthenticatedUser,
  space: SpaceId,
  request: CreateInviteRequest,
) -> Result<InviteResponse, ApiError> {
  let access = load_space(state, caller, space).await?;
  if matches!(request.role, SpaceRole::Unknown(_)) {
    return Err(ApiError::InvalidRequest("unknown space role".into()));
  }
  require_member_management(state, caller, &request.role, &access.target())?;
  if access.view.space.is_archived {
    return Err(ApiError::Conflict("the space is archived".into()));
  }

  let email = normalize_email(&request.email)?;
  let limits = &state.settings.get().limits;
  let maybe_message = request
    .maybe_message
    .as_deref()
    .map(str::trim)
    .filter(|message| !message.is_empty())
    .map(|message| {
      super::validate_text(message, "invite message", limits.max_text_length).map(|()| message.to_owned())
    })
    .transpose()?;
  if request.role == SpaceRole::Guest {
    check_guest_domain(state, &email)?;
  }
  check_channels(state, caller, space, &request.channel_ids).await?;
  let maybe_expires_at = future_expiry(request.maybe_expires_at)?;
  if let Some(at) = maybe_expires_at {
    super::guests::check_new_expiry(state, caller, &access.target(), at)?;
  }

  let invite = state
    .database
    .create_invite(&NewInvite {
      id: state.next_id()?,
      space_id: space,
      email: &email,
      role: request.role,
      channel_ids: &request.channel_ids,
      sponsor: caller.user.id,
      maybe_expires_at,
    })
    .await?
    .ok_or(ApiError::InviteAlreadyOpen)?;
  let invite_brand = invite_brand(state, caller, &invite.email).await;
  let should_send_mail = match state.mailer_for(&invite_brand) {
    Some(_) => match spend_invite_mail(state, caller) {
      Ok(has_budget) => has_budget,
      Err(over_budget) => {
        state.database.delete_invite(invite.id).await?;
        return Err(over_budget);
      },
    },
    None => false,
  };
  audit::record(
    state,
    Some(caller.user.id),
    "invite_created",
    Some(AuditTarget::new("space", space)),
    &json!({"role": invite.role.as_str()}),
  )
  .await?;
  let mail = if should_send_mail {
    send_invite_mail(
      state,
      caller,
      &invite_brand,
      &access.view.space.name,
      &invite,
      maybe_message.as_deref(),
    )
    .await
  } else {
    InviteMail::NoMailServer
  };
  Ok(InviteResponse {
    maybe_mail: Some(mail),
    ..invite_response(&invite)
  })
}

/// The channels an invite lists must be channels of the space.
pub async fn check_channels(
  state: &AppState,
  caller: &AuthenticatedUser,
  space: SpaceId,
  channels: &[teamcraft_proto::ConversationId],
) -> Result<(), ApiError> {
  if channels.len() > MAX_INVITE_CHANNELS {
    return Err(ApiError::InvalidRequest("too many channels".into()));
  }
  for channel in channels {
    let view = state.database.get_conversation(*channel, caller.user.id).await?;
    let belongs = view.is_some_and(|v| {
      v.conversation.maybe_space_id == Some(space) && v.conversation.kind == ConversationKind::Channel
    });
    if !belongs {
      return Err(ApiError::InvalidRequest(
        "channel_ids must be channels of this space".into(),
      ));
    }
  }
  Ok(())
}

/// An optional expiry given by a client; it must lie in the future.
pub fn future_expiry(
  maybe_millis: Option<teamcraft_proto::UnixMillis>,
) -> Result<Option<OffsetDateTime>, ApiError> {
  let Some(millis) = maybe_millis else {
    return Ok(None);
  };
  let at = offset_date_time(millis).ok_or_else(|| ApiError::InvalidRequest("invalid expires_at".into()))?;
  if at <= OffsetDateTime::now_utc() {
    return Err(ApiError::InvalidRequest(
      "expires_at must be in the future".into(),
    ));
  }
  Ok(Some(at))
}

/// The inviter's home brand (their e-mail domain, else the default brand). Mail to a domain no brand
/// lists goes out as this brand, never as the brand of the host the request came in on: whoever
/// controls the `Host` header must not choose the identity of mail the server sends.
pub(super) async fn inviter_brand(state: &AppState, user: &UserId) -> Arc<Brand> {
  let maybe_email = match state.database.get_user_emails(&[*user]).await {
    Ok(emails) => emails.get(user).cloned(),
    Err(error) => {
      warn!(%error, "could not read the inviter's e-mail address; using the default brand");
      None
    },
  };
  let brands = state.brands.current();
  Arc::clone(
    maybe_email
      .as_deref()
      .map_or(brands.default_brand(), |email| brands.for_email(email)),
  )
}

/// Mails the invitee, if the server has a mail server. A failure is logged and does not undo the
/// invite (it is applied at sign-in anyway); the outcome goes into the answer, so that the inviter
/// learns that the mail did not go out.
async fn send_invite_mail(
  state: &AppState,
  caller: &AuthenticatedUser,
  brand: &Brand,
  space_name: &str,
  invite: &teamcraft_db::Invite,
  maybe_message: Option<&str>,
) -> InviteMail {
  let Some(mailer) = state.mailer_for(brand) else {
    return InviteMail::NoMailServer;
  };
  let mail = invite_mail(state, caller, brand, space_name, invite, maybe_message);
  match mailer.send(&mail).await {
    Ok(()) => InviteMail::Sent,
    Err(error) => {
      warn!(%error, invite_id = %invite.id, "sending the invitation e-mail failed");
      InviteMail::NotSent
    },
  }
}

/// The brand an invitation goes out as: that of the recipient's domain, else the inviter's.
async fn invite_brand(state: &AppState, caller: &AuthenticatedUser, email: &str) -> Arc<Brand> {
  let inviter_brand = inviter_brand(state, &caller.user.id).await;
  let brands = state.brands.current();
  Arc::clone(brands.for_recipient(email, &inviter_brand))
}

fn invite_mail(
  state: &AppState,
  caller: &AuthenticatedUser,
  brand: &Brand,
  space_name: &str,
  invite: &teamcraft_db::Invite,
  maybe_message: Option<&str>,
) -> OutgoingMail {
  // `/home` and not the bare address: the apps claim places, and the bare address is also where a
  // browser's sign-in comes back to (docs/app-links.md).
  let url = format!(
    "{}/home",
    brand.base_url(&state.settings.get().mail.public_base_url)
  );
  let (subject, body) = brand.invite_texts(&state.settings.get().mail);
  let fill = |template: &str| {
    template
      .replace(
        "{inviter}",
        &presented_name(&state.policy.get().directory, &caller.user),
      )
      .replace("{space}", space_name)
      .replace("{role}", invite.role.as_str())
      .replace("{url}", &url)
  };
  OutgoingMail {
    to_address: invite.email.clone(),
    maybe_to_name: None,
    subject: fill(&subject),
    text: with_personal_message(&fill(&body), maybe_message),
    method: Method::Request,
    ics: String::new(),
    maybe_from_name: None,
    maybe_from_address: None,
  }
  .as_brand(brand)
}

/// Charges one invitation mail to the caller's hourly mail budget, shared with their meetings: the
/// instance's mail identity is not for bulk mail. `false` when the budget is 0, which means no mail,
/// and refused while it is used up.
fn spend_invite_mail(state: &AppState, caller: &AuthenticatedUser) -> Result<bool, ApiError> {
  let budget = state.settings.get().limits.max_meeting_mails_per_user_per_hour;
  if budget == 0 {
    return Ok(false);
  }
  if state.rate_limits.spend_mails(caller.user.id, 1, budget) {
    Ok(true)
  } else {
    Err(ApiError::RateLimited {
      retry_after_secs: 3600 / u64::from(budget),
    })
  }
}

/// Adds the inviter's own words to the invitation text: in place of `{message}` when the template
/// has it, else quoted below. Inserted after the other placeholders are filled, so text in the
/// message is never taken for a placeholder.
fn with_personal_message(text: &str, maybe_message: Option<&str>) -> String {
  let quoted = maybe_message.map(|message| {
    message
      .lines()
      .map(|line| format!("> {line}"))
      .collect::<Vec<_>>()
      .join("\n")
  });
  match quoted {
    Some(quoted) if text.contains("{message}") => text.replace("{message}", &quoted),
    Some(quoted) => format!("{text}\n\n{quoted}\n"),
    None => text.replace("{message}", ""),
  }
}

pub async fn list_invites(
  state: &AppState,
  caller: &AuthenticatedUser,
  space: SpaceId,
) -> Result<ListInvitesResponse, ApiError> {
  let access = load_space(state, caller, space).await?;
  super::access::require(state, caller, Permission::ManageSpaceMembers, &access.target())?;
  let invites = state.database.list_pending_invites(space).await?;
  Ok(ListInvitesResponse {
    invites: invites.iter().map(invite_response).collect(),
  })
}

pub async fn delete_invite(
  state: &AppState,
  caller: &AuthenticatedUser,
  space: SpaceId,
  invite: InviteId,
) -> Result<(), ApiError> {
  let access = load_space(state, caller, space).await?;
  manageable_invite(state, caller, &access.target(), space, invite).await?;
  state.database.delete_invite(invite).await?;
  audit::record(
    state,
    Some(caller.user.id),
    "invite_revoked",
    Some(AuditTarget::new("space", space)),
    &json!({}),
  )
  .await?;
  Ok(())
}

/// Sends the invitation mail again, at most once per `limits.invite_resend_interval_secs`. Unlike
/// the mail of a new invitation, a failure here is the caller's answer: sending is all this does.
pub async fn resend_invite(
  state: &AppState,
  caller: &AuthenticatedUser,
  space: SpaceId,
  invite: InviteId,
) -> Result<(), ApiError> {
  let access = load_space(state, caller, space).await?;
  let existing = manageable_invite(state, caller, &access.target(), space, invite).await?;
  if access.view.space.is_archived {
    return Err(ApiError::Conflict("the space is archived".into()));
  }
  if is_expired(existing.maybe_expires_at) {
    return Err(ApiError::Conflict("the invitation has expired".into()));
  }
  if existing.role == SpaceRole::Guest {
    check_guest_domain(state, &existing.email)?;
  }
  let brand = invite_brand(state, caller, &existing.email).await;
  let Some(mailer) = state.mailer_for(&brand) else {
    return Err(ApiError::Conflict("this server sends no mail".into()));
  };
  let interval_secs = state.settings.get().limits.invite_resend_interval_secs;
  if !state.database.mark_invite_resent(invite, interval_secs).await? {
    return Err(ApiError::RateLimited {
      retry_after_secs: interval_secs,
    });
  }
  // A refusal for the budget sends nothing, so it must not start the interval either. The interval
  // goes first so that its refusal doesn't use up the budget.
  let refusal = match spend_invite_mail(state, caller) {
    Ok(true) => None,
    Ok(false) => Some(ApiError::Conflict("this server sends no invitation mail".into())),
    Err(over_budget) => Some(over_budget),
  };
  if let Some(refusal) = refusal {
    state.database.forget_invite_resend(invite).await?;
    return Err(refusal);
  }
  let mail = invite_mail(state, caller, &brand, &access.view.space.name, &existing, None);
  if let Err(error) = mailer.send(&mail).await {
    warn!(%error, invite_id = %invite, "resending the invitation e-mail failed");
    state.database.forget_invite_resend(invite).await?;
    return Err(ApiError::Unavailable(
      "the mail server did not take the mail".into(),
    ));
  }
  audit::record(
    state,
    Some(caller.user.id),
    "invite_resent",
    Some(AuditTarget::new("space", space)),
    &json!({"role": existing.role.as_str()}),
  )
  .await?;
  Ok(())
}

/// One page of the open invitations the caller may manage, across spaces.
pub async fn list_directory_invitations(
  state: &AppState,
  caller: &AuthenticatedUser,
  query: &DirectoryInvitationsQuery,
) -> Result<DirectoryInvitationsResponse, ApiError> {
  if is_guest(caller) {
    return Err(ApiError::Forbidden(
      "invitations are not available to guests".into(),
    ));
  }
  let listing = if is_instance_admin(caller) {
    SpaceListing::All
  } else {
    SpaceListing::Mine
  };
  let spaces = state
    .database
    .list_spaces_for_user(caller.user.id, listing)
    .await?;
  let (mut managed_spaces, mut guest_inviter_spaces) = (Vec::new(), Vec::new());
  for space in &spaces {
    let reach = InviteReach::of(
      state,
      caller,
      &space_target(Some(space.space.id), space.maybe_role.clone()),
    );
    if reach.manages_all {
      managed_spaces.push(space.space.id);
    } else if reach.has_own_guests {
      guest_inviter_spaces.push(space.space.id);
    }
  }
  check_search_text(state, &query.query)?;
  let page_size = page_size(state);
  let mut found = state
    .database
    .directory_invitations(&DirectoryInvitationFilter {
      sponsor: caller.user.id,
      managed_spaces: &managed_spaces,
      guest_inviter_spaces: &guest_inviter_spaces,
      query: query.query.trim(),
      maybe_space: query.maybe_space_id,
      maybe_after: query.maybe_after,
      limit: page_size + 1,
    })
    .await?;
  let has_more = found.len() as i64 > page_size;
  found.truncate(page_size as usize);
  let maybe_next_after = has_more.then(|| found.last().map(|invite| invite.id)).flatten();
  let sponsor_ids: Vec<UserId> = found.iter().map(|invite| invite.sponsor_user_id).collect();
  let sponsors = state.database.get_users(&sponsor_ids).await?;
  let has_mail_budget = state.settings.get().limits.max_meeting_mails_per_user_per_hour > 0;
  let resender_brand = inviter_brand(state, &caller.user.id).await;
  let brands = state.brands.current();
  let mut invitations = Vec::with_capacity(found.len());
  for invite in found {
    let sponsor = sponsors
      .iter()
      .find(|user| user.id == invite.sponsor_user_id)
      .ok_or_else(|| ApiError::Internal("invite sponsor missing".into()))?;
    let is_expired = is_expired(invite.maybe_expires_at);
    let can_send_mail = has_mail_budget
      && state
        .mailer_for(brands.for_recipient(&invite.email, &resender_brand))
        .is_some();
    invitations.push(DirectoryInvitation {
      id: invite.id,
      space_id: invite.space_id,
      space_name: invite.space_name,
      email: invite.email,
      role: invite.role,
      sponsor: user_summary(state, sponsor),
      maybe_expires_at: invite.maybe_expires_at.map(unix_millis),
      is_expired,
      can_resend: can_send_mail && !is_expired && !invite.is_space_archived,
    });
  }
  Ok(DirectoryInvitationsResponse {
    invitations,
    maybe_next_after,
  })
}

/// The open invitation, if the caller may manage it; one they may not manage doesn't exist for
/// them, as it isn't in their list either.
async fn manageable_invite(
  state: &AppState,
  caller: &AuthenticatedUser,
  target: &Target,
  space: SpaceId,
  invite: InviteId,
) -> Result<teamcraft_db::Invite, ApiError> {
  let reach = InviteReach::of(state, caller, target);
  if !reach.manages_all && !reach.has_own_guests {
    return Err(ApiError::Forbidden(format!(
      "missing permission {}",
      Permission::ManageSpaceMembers
    )));
  }
  state
    .database
    .get_invite(invite)
    .await?
    .filter(|existing| {
      existing.space_id == space && reach.covers(caller.user.id, &existing.role, existing.sponsor_user_id)
    })
    .ok_or_else(ApiError::invite_not_found)
}

fn is_expired(maybe_expires_at: Option<OffsetDateTime>) -> bool {
  maybe_expires_at.is_some_and(|at| at <= OffsetDateTime::now_utc())
}

/// Guests may only be invited from the configured domains, if there are any.
pub fn check_guest_domain(state: &AppState, email: &str) -> Result<(), ApiError> {
  let policy = state.policy.get();
  let is_allowed = [
    &state.settings.get().guests.allowed_email_domains,
    &policy.guests.allowed_email_domains,
  ]
  .into_iter()
  .all(|allowed| is_domain_allowed(allowed, email));
  if is_allowed {
    Ok(())
  } else {
    Err(ApiError::InvalidRequest(
      "guests from this e-mail domain are not allowed".into(),
    ))
  }
}

/// Whether `email` belongs to one of the domains (or their subdomains); an empty list allows all.
pub fn is_domain_allowed(allowed: &[String], email: &str) -> bool {
  if allowed.is_empty() {
    return true;
  }
  let domain = email
    .rsplit_once('@')
    .map_or("", |(_, domain)| domain)
    .to_lowercase();
  allowed.iter().any(|candidate| {
    let candidate = candidate.trim_start_matches('@').to_lowercase();
    domain == candidate || domain.ends_with(&format!(".{candidate}"))
  })
}

/// Lowercases and sanity-checks an email address; real verification is the identity provider's job.
pub fn normalize_email(email: &str) -> Result<String, ApiError> {
  let email = email.trim().to_lowercase();
  let valid = email.len() <= MAX_EMAIL_CHARS
    && !email.chars().any(|c| c.is_whitespace() || c.is_control())
    && email.split_once('@').is_some_and(|(local, domain)| {
      !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains('@')
    });
  if valid {
    Ok(email)
  } else {
    Err(ApiError::InvalidRequest("invalid email address".into()))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn emails_are_normalized_and_checked() {
    assert_eq!(
      normalize_email("  Guest@Example.COM ").unwrap(),
      "guest@example.com"
    );
    for bad in [
      "",
      "no-at.example.com",
      "@example.com",
      "a@b",
      "a@@b.com",
      "a b@example.com",
      "a@.com",
      "a@b.com.",
    ] {
      assert!(normalize_email(bad).is_err(), "{bad:?}");
    }
  }
}

#[cfg(test)]
mod reach_tests {
  use super::*;

  fn user(raw: u64) -> UserId {
    raw.to_string().parse().unwrap()
  }

  #[test]
  fn a_guest_inviter_reaches_only_the_guests_they_invited() {
    let reach = InviteReach {
      manages_all: false,
      has_own_guests: true,
    };
    assert!(reach.covers(user(1), &SpaceRole::Guest, user(1)));
    assert!(
      !reach.covers(user(1), &SpaceRole::Guest, user(2)),
      "somebody else's"
    );
    assert!(!reach.covers(user(1), &SpaceRole::Member, user(1)), "not a guest");
  }

  #[test]
  fn a_member_manager_reaches_every_invitation_and_nobody_else_any() {
    let manager = InviteReach {
      manages_all: true,
      has_own_guests: false,
    };
    assert!(manager.covers(user(1), &SpaceRole::Member, user(2)));
    let none = InviteReach {
      manages_all: false,
      has_own_guests: false,
    };
    assert!(!none.covers(user(1), &SpaceRole::Guest, user(1)));
  }
}

#[cfg(test)]
mod message_tests {
  use super::*;

  #[test]
  fn a_personal_message_is_quoted_below_the_invitation() {
    assert_eq!(
      with_personal_message("Join us.", Some("Hi!\nSee you there")),
      "Join us.\n\n> Hi!\n> See you there\n"
    );
  }

  #[test]
  fn a_template_can_place_the_message() {
    assert_eq!(
      with_personal_message("Hello\n{message}\nBye", Some("Welcome")),
      "Hello\n> Welcome\nBye"
    );
    assert_eq!(with_personal_message("Hello{message}", None), "Hello");
  }

  #[test]
  fn placeholders_in_the_message_stay_text() {
    let filled = with_personal_message("Open {url}", Some("{url}"));
    assert!(filled.ends_with("> {url}\n"), "{filled}");
  }
}

#[cfg(test)]
mod domain_tests {
  use super::*;

  #[test]
  fn domains_match_exactly_or_as_subdomains_without_case() {
    let allowed = vec!["Example.org".to_owned(), "@partner.test".to_owned()];
    assert!(is_domain_allowed(&allowed, "a@example.org"));
    assert!(is_domain_allowed(&allowed, "a@Team.Example.ORG"));
    assert!(is_domain_allowed(&allowed, "a@partner.test"));
    assert!(!is_domain_allowed(&allowed, "a@notexample.org"));
    assert!(!is_domain_allowed(&allowed, "a@example.org.evil.test"));
    assert!(!is_domain_allowed(&allowed, "no-at-sign"));
    assert!(is_domain_allowed(&[], "anyone@anywhere.test"));
  }
}
