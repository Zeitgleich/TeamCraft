//! Guest lifecycle (PLAN.md §12, issue #212): guests expire, they and their sponsor hear about it
//! beforehand, and who may extend them is decided here (admin.md §3.2.2).

use std::collections::{HashMap, HashSet};

use serde_json::json;
use teamcraft_db::{AdminGuestListFilter, ExpiryChange, GuestNoticeDue, NewSpaceMember, User};
use teamcraft_proto::{
  AdminGuestFilter, AdminGuestResponse, ExpiringGuestResponse, ExtendGuestRequest, GatewayEvent,
  ListAdminGuestsQuery, ListAdminGuestsResponse, ListExpiringGuestsResponse, Permission, SpaceId,
  SpaceMemberResponse, SpaceRole, Target, UserId,
};
use time::{Duration, OffsetDateTime};
use tracing::warn;

use super::access::{SpaceAccess, allows, is_guest, load_space, require, space_target};
use super::audit::{self, Target as AuditTarget};
use super::convert::{offset_date_time, unix_millis, user_summary};
use super::spaces::remove_everywhere;
use super::workspace::{MAX_GUEST_EXPIRY_DAYS, current_guest_policy};
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::meetings::ical::Method;
use crate::meetings::mailer::OutgoingMail;
use crate::state::AppState;

/// Guests handled per run; a larger backlog continues at the next run.
const BATCH: i64 = 200;
/// Guests one person's "ending soon" list shows.
const EXPIRING_LIST_LIMIT: i64 = 100;

/// How far a caller may extend a guest's membership.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionRight {
  None,
  UpTo(OffsetDateTime),
  Unlimited,
}

/// Who may extend a guest, in one place (admin.md §3.2.2): whoever manages the space's members
/// without limit, the guest's sponsor who still belongs to the space up to the policy's limit, and
/// never a guest, not even one somebody made a sponsor.
fn extension_right(
  is_caller_guest: bool,
  manages_members: bool,
  is_sponsor_in_space: bool,
  sponsor_days: u32,
  now: OffsetDateTime,
) -> ExtensionRight {
  if is_caller_guest {
    ExtensionRight::None
  } else if manages_members {
    ExtensionRight::Unlimited
  } else if is_sponsor_in_space && sponsor_days > 0 {
    ExtensionRight::UpTo(now + Duration::days(i64::from(sponsor_days)))
  } else {
    ExtensionRight::None
  }
}

/// The caller's right on a guest in a space, from the permission evaluator and the policy.
fn right_of(
  state: &AppState,
  caller: &AuthenticatedUser,
  target: &Target,
  maybe_sponsor: Option<UserId>,
  is_in_space: bool,
  now: OffsetDateTime,
) -> ExtensionRight {
  extension_right(
    is_guest(caller),
    allows(state, caller, &Permission::ManageSpaceMembers, target),
    maybe_sponsor == Some(caller.user.id) && is_in_space,
    current_guest_policy(state).effective_sponsor_extension_days(),
    now,
  )
}

/// Extends a guest: an absolute time, so that a retry changes nothing. Never shortens.
pub async fn extend(
  state: &AppState,
  caller: &AuthenticatedUser,
  space: SpaceId,
  user_id: UserId,
  request: ExtendGuestRequest,
) -> Result<SpaceMemberResponse, ApiError> {
  let access = load_space(state, caller, space).await?;
  if state.database.space_role(space, user_id).await? != Some(SpaceRole::Guest) {
    return Err(ApiError::user_not_found());
  }
  let user = state
    .database
    .get_user(user_id)
    .await?
    .ok_or_else(ApiError::user_not_found)?;
  let (maybe_expires, maybe_sponsor) = state
    .database
    .space_member_terms(space, user_id)
    .await?
    .ok_or_else(ApiError::user_not_found)?;
  let now = OffsetDateTime::now_utc();
  let at = offset_date_time(request.expires_at)
    .filter(|at| *at > now && *at <= now + Duration::days(i64::from(MAX_GUEST_EXPIRY_DAYS)))
    .ok_or_else(|| ApiError::InvalidRequest("expires_at must be in the future, within ten years".into()))?;
  let Some(current) = maybe_expires else {
    return Err(ApiError::Conflict("this guest does not expire".into()));
  };
  if at < current {
    return Err(ApiError::InvalidRequest(
      "extending can't shorten the membership".into(),
    ));
  }
  let right = right_of(
    state,
    caller,
    &access.target(),
    maybe_sponsor,
    access.view.maybe_role.is_some(),
    now,
  );
  ensure_within(right, at)?;

  // The same time again is a retry after a lost response: nothing changes, nobody hears twice.
  if at != current {
    state
      .database
      .upsert_space_member(&NewSpaceMember {
        space,
        user: user_id,
        role: SpaceRole::Guest,
        expiry: ExpiryChange::Set(at),
        maybe_sponsor: None,
      })
      .await?;
    audit::record(
      state,
      Some(caller.user.id),
      "guest_extended",
      Some(AuditTarget::new("space", space)),
      &json!({"user_id": user_id.to_string(), "expires_at": unix_millis(at).0}),
    )
    .await?;
    publish_terms(state, space, user_id, maybe_sponsor).await?;
  }
  Ok(SpaceMemberResponse {
    user: user_summary(state, &user),
    role: SpaceRole::Guest,
    maybe_expires_at: Some(unix_millis(at)),
    maybe_sponsor_user_id: maybe_sponsor,
  })
}

/// The limits also hold for the general member update: only somebody without limit clears an
/// expiry, whatever the permission scheme lets them do otherwise.
pub(super) async fn check_expiry_change(
  state: &AppState,
  caller: &AuthenticatedUser,
  access: &SpaceAccess,
  user_id: UserId,
  change: ExpiryChange,
) -> Result<(), ApiError> {
  if change == ExpiryChange::Keep {
    return Ok(());
  }
  let maybe_sponsor = state
    .database
    .space_member_terms(access.view.space.id, user_id)
    .await?
    .and_then(|(_, sponsor)| sponsor);
  let now = OffsetDateTime::now_utc();
  let right = right_of(
    state,
    caller,
    &access.target(),
    maybe_sponsor,
    access.view.maybe_role.is_some(),
    now,
  );
  match change {
    ExpiryChange::Set(at) => ensure_within(right, at),
    ExpiryChange::Clear if right == ExtensionRight::Unlimited => Ok(()),
    ExpiryChange::Clear | ExpiryChange::Keep => Err(ApiError::Forbidden(
      "you may not change this guest's expiry".into(),
    )),
  }
}

fn ensure_within(right: ExtensionRight, at: OffsetDateTime) -> Result<(), ApiError> {
  match right {
    ExtensionRight::Unlimited => Ok(()),
    ExtensionRight::UpTo(limit) if at <= limit => Ok(()),
    ExtensionRight::UpTo(_) => Err(ApiError::Forbidden(
      "that is further than sponsors may extend".into(),
    )),
    ExtensionRight::None => Err(ApiError::Forbidden("you may not extend this guest".into())),
  }
}

/// Guests ending soon that concern the caller, for the notice in the app: it stands until the
/// expiry changes or passes, also for somebody who was offline when the notice went out.
pub async fn list_expiring(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<ListExpiringGuestsResponse, ApiError> {
  let policy = current_guest_policy(state);
  let Some(longest) = policy.effective_notice_days().into_iter().max() else {
    return Ok(ListExpiringGuestsResponse { guests: Vec::new() });
  };
  let now = OffsetDateTime::now_utc();
  let until = now + Duration::days(i64::from(longest));
  let rows = state
    .database
    .list_expiring_guests_for(caller.user.id, now, until, EXPIRING_LIST_LIMIT)
    .await?;
  let people = users_by_id(state, rows.iter().map(|row| row.user)).await?;
  let guests = rows
    .into_iter()
    .filter_map(|row| {
      let user = people.get(&row.user)?;
      let target = space_target(Some(row.space), row.maybe_viewer_role.clone());
      let is_in_space = row.maybe_viewer_role.is_some();
      let right = if row.user == caller.user.id {
        ExtensionRight::None
      } else {
        right_of(state, caller, &target, row.maybe_sponsor, is_in_space, now)
      };
      Some(ExpiringGuestResponse {
        space_id: row.space,
        space_name: row.space_name,
        user: user_summary(state, user),
        expires_at: unix_millis(row.expires_at),
        is_own: row.user == caller.user.id,
        can_extend: right != ExtensionRight::None,
        maybe_extend_limit: match right {
          ExtensionRight::UpTo(limit) => Some(unix_millis(limit)),
          ExtensionRight::None | ExtensionRight::Unlimited => None,
        },
      })
    })
    .collect();
  Ok(ListExpiringGuestsResponse { guests })
}

/// The admin console's overview of guest memberships.
pub async fn admin_list(
  state: &AppState,
  caller: &AuthenticatedUser,
  query: ListAdminGuestsQuery,
) -> Result<ListAdminGuestsResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  let limits = &state.settings.get().limits;
  let limit = query
    .maybe_limit
    .unwrap_or(limits.default_page_size)
    .clamp(1, limits.max_page_size);
  let longest = current_guest_policy(state)
    .effective_notice_days()
    .into_iter()
    .max()
    .unwrap_or(0);
  let now = OffsetDateTime::now_utc();
  let mut rows = state
    .database
    .admin_list_guests(&AdminGuestListFilter {
      maybe_space: query.maybe_space_id,
      maybe_sponsor: query.maybe_sponsor_id,
      maybe_expires_by: (query.maybe_filter == Some(AdminGuestFilter::ExpiringSoon))
        .then(|| now + Duration::days(i64::from(longest))),
      has_no_sponsor: query.maybe_filter == Some(AdminGuestFilter::NoSponsor),
      has_never_signed_in: query.maybe_filter == Some(AdminGuestFilter::NeverSignedIn),
      limit: i64::from(limit) + 1,
    })
    .await?;
  let has_more = rows.len() > limit as usize;
  rows.truncate(limit as usize);
  let people = users_by_id(
    state,
    rows
      .iter()
      .flat_map(|row| std::iter::once(row.user).chain(row.maybe_sponsor)),
  )
  .await?;
  let guests = rows
    .into_iter()
    .filter_map(|row| {
      Some(AdminGuestResponse {
        user: user_summary(state, people.get(&row.user)?),
        space_id: row.space,
        space_name: row.space_name,
        maybe_expires_at: row.maybe_expires_at.map(unix_millis),
        maybe_sponsor: row
          .maybe_sponsor
          .and_then(|sponsor| people.get(&sponsor))
          .map(|sponsor| user_summary(state, sponsor)),
        has_signed_in: row.has_signed_in,
      })
    })
    .collect();
  Ok(ListAdminGuestsResponse { guests, has_more })
}

async fn users_by_id(
  state: &AppState,
  ids: impl Iterator<Item = UserId>,
) -> Result<HashMap<UserId, User>, ApiError> {
  let unique: Vec<UserId> = ids.collect::<HashSet<_>>().into_iter().collect();
  let users = state.database.get_users(&unique).await?;
  Ok(users.into_iter().map(|user| (user.id, user)).collect())
}

/// Sends the notices due at `now`: an event for whoever is online, whose apps read the list again,
/// and one mail per person. The mails name no guest and no space. A lost mail is not sent again:
/// the notice in the app stands until the expiry. Returns how many memberships were noticed.
pub async fn send_notices_once(state: &AppState, now: OffsetDateTime) -> Result<usize, ApiError> {
  let lead_days: Vec<i32> = current_guest_policy(state)
    .effective_notice_days()
    .into_iter()
    .filter_map(|days| i32::try_from(days).ok())
    .collect();
  if lead_days.is_empty() {
    return Ok(0);
  }
  let due = state
    .database
    .list_guests_due_notice(now, &lead_days, BATCH)
    .await?;
  let mut sponsored: HashMap<UserId, u32> = HashMap::new();
  let mut guests: HashMap<UserId, i32> = HashMap::new();
  for notice in &due {
    state.database.record_guest_notice(notice).await?;
    publish_terms(state, notice.space, notice.user, notice.maybe_sponsor).await?;
    if let Some(sponsor) = notice.maybe_sponsor {
      *sponsored.entry(sponsor).or_default() += 1;
    }
    let days = days_left(notice, now);
    guests
      .entry(notice.user)
      .and_modify(|left| *left = days.min(*left))
      .or_insert(days);
  }
  mail_notices(state, &sponsored, &guests).await?;
  Ok(due.len())
}

fn days_left(notice: &GuestNoticeDue, now: OffsetDateTime) -> i32 {
  let hours = (notice.expires_at - now).whole_hours();
  // Rounded up: "in 3 days" for a guest with 71 hours left, never "in 0 days".
  i32::try_from((hours + 23) / 24).unwrap_or(i32::MAX).max(1)
}

async fn mail_notices(
  state: &AppState,
  sponsored: &HashMap<UserId, u32>,
  guests: &HashMap<UserId, i32>,
) -> Result<(), ApiError> {
  let Some(mailer) = state.mailer.get() else {
    return Ok(());
  };
  let people: Vec<UserId> = sponsored.keys().chain(guests.keys()).copied().collect();
  let emails = state.database.get_user_emails(&people).await?;
  let settings = state.settings.get();
  let config = &settings.guests;
  let brands = state.brands.current();
  let mut outgoing = Vec::new();
  for (sponsor, count) in sponsored {
    outgoing.push((
      *sponsor,
      &config.notice_subject_sponsor,
      &config.notice_body_sponsor,
      count.to_string(),
      String::new(),
    ));
  }
  for (guest, days) in guests {
    outgoing.push((
      *guest,
      &config.notice_subject_guest,
      &config.notice_body_guest,
      String::new(),
      days.to_string(),
    ));
  }
  for (user, subject, body, count, days) in outgoing {
    let Some(address) = emails.get(&user) else {
      continue;
    };
    let brand = brands.for_email(address);
    let url = brand.base_url(&settings.mail.public_base_url);
    let mail = OutgoingMail {
      to_address: address.clone(),
      maybe_to_name: None,
      subject: subject.clone(),
      text: body
        .replace("{count}", &count)
        .replace("{days}", &days)
        .replace("{url}", &url),
      method: Method::Request,
      ics: String::new(),
      maybe_from_name: None,
      maybe_from_address: None,
    }
    .as_brand(brand);
    if let Err(error) = mailer.send(&mail).await {
      warn!(%error, user_id = %user, "mailing a guest expiry notice failed");
    }
  }
  Ok(())
}

/// Tells the guest, the sponsor and the space's admins to read the list again.
async fn publish_terms(
  state: &AppState,
  space: SpaceId,
  user: UserId,
  maybe_sponsor: Option<UserId>,
) -> Result<(), ApiError> {
  let mut audience: Vec<UserId> = state
    .database
    .list_space_members(space)
    .await?
    .into_iter()
    .filter(|member| member.role == SpaceRole::SpaceAdmin)
    .map(|member| member.user.id)
    .collect();
  audience.push(user);
  audience.extend(maybe_sponsor);
  state.hub.publish(
    audience,
    &GatewayEvent::GuestTermsUpdate {
      space_id: space,
      user_id: user,
    },
  );
  Ok(())
}

/// Removes guests whose membership expired at `now` from their space and its channels, and tells
/// the sponsor and the space's admins. Returns how many were removed.
pub async fn expire_once(state: &AppState, now: OffsetDateTime) -> Result<usize, ApiError> {
  let expired = state.database.list_expired_guests(now, BATCH).await?;
  let mut removed = 0;
  for guest in expired {
    let Some(user) = state.database.get_user(guest.user).await? else {
      continue;
    };
    // Admins to inform: the space's admins plus the sponsor (who may have left or been demoted).
    let mut audience: Vec<_> = state
      .database
      .list_space_members(guest.space)
      .await?
      .into_iter()
      .filter(|member| member.role == SpaceRole::SpaceAdmin)
      .map(|member| member.user.id)
      .collect();
    audience.extend(guest.maybe_sponsor);

    if let Err(error) = remove_everywhere(state, guest.space, guest.user, false).await {
      warn!(%error, space_id = %guest.space, "removing an expired guest failed");
      continue;
    }
    audit::record(
      state,
      None,
      "guest_expired",
      Some(AuditTarget::new("space", guest.space)),
      &json!({"user_id": guest.user.to_string()}),
    )
    .await?;
    state.hub.publish(
      audience,
      &GatewayEvent::GuestExpired {
        space_id: guest.space,
        user: user_summary(state, &user),
      },
    );
    removed += 1;
  }
  Ok(removed)
}

#[cfg(test)]
mod tests {
  use super::*;

  const NOW: OffsetDateTime = time::macros::datetime!(2026-11-05 12:00 UTC);

  /// Every role that matters × extending, guests especially (testing.md).
  #[test]
  fn who_may_extend_a_guest() {
    let up_to_90 = ExtensionRight::UpTo(NOW + Duration::days(90));
    let cases = [
      // (is a guest, manages members, sponsor in the space, sponsor days) => right
      (
        "space or workspace admin",
        false,
        true,
        false,
        90,
        ExtensionRight::Unlimited,
      ),
      (
        "admin who is the sponsor",
        false,
        true,
        true,
        90,
        ExtensionRight::Unlimited,
      ),
      (
        "admin, sponsors may not extend",
        false,
        true,
        false,
        0,
        ExtensionRight::Unlimited,
      ),
      ("sponsor", false, false, true, 90, up_to_90),
      (
        "sponsor, policy says no",
        false,
        false,
        true,
        0,
        ExtensionRight::None,
      ),
      (
        "member, not the sponsor",
        false,
        false,
        false,
        90,
        ExtensionRight::None,
      ),
      ("guest", true, false, false, 90, ExtensionRight::None),
      (
        "guest who is a sponsor",
        true,
        false,
        true,
        90,
        ExtensionRight::None,
      ),
      (
        "guest whose scheme says admin",
        true,
        true,
        true,
        90,
        ExtensionRight::None,
      ),
    ];
    for (who, is_guest, manages, is_sponsor, days, expected) in cases {
      assert_eq!(
        extension_right(is_guest, manages, is_sponsor, days, NOW),
        expected,
        "{who}"
      );
    }
  }

  #[test]
  fn a_sponsor_stays_within_the_cap() {
    let limit = NOW + Duration::days(90);
    assert!(ensure_within(ExtensionRight::UpTo(limit), limit).is_ok());
    assert!(ensure_within(ExtensionRight::UpTo(limit), limit + Duration::seconds(1)).is_err());
    assert!(ensure_within(ExtensionRight::None, NOW).is_err());
    assert!(ensure_within(ExtensionRight::Unlimited, limit + Duration::days(1000)).is_ok());
  }

  #[test]
  fn days_left_round_up_and_never_say_zero() {
    let due = |hours| GuestNoticeDue {
      space: "1".parse().unwrap(),
      user: "2".parse().unwrap(),
      maybe_sponsor: None,
      expires_at: NOW + Duration::hours(hours),
      lead_days: 3,
    };
    assert_eq!(days_left(&due(72), NOW), 3);
    assert_eq!(days_left(&due(71), NOW), 3);
    assert_eq!(days_left(&due(49), NOW), 3);
    assert_eq!(days_left(&due(1), NOW), 1);
    assert_eq!(days_left(&due(0), NOW), 1);
  }
}
