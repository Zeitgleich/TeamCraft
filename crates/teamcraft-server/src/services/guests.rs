//! Guest lifecycle (PLAN.md §12, issue #212): guests expire, they and their sponsor hear about it
//! beforehand, and who may extend them is decided here (admin.md §3.2.2).

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use serde_json::json;
use teamcraft_db::{AdminGuestListFilter, ExpiryChange, GuestNoticeDue, SpaceMember, User};
use teamcraft_proto::{
  AdminGuestFilter, AdminGuestResponse, ExpiringGuestResponse, ExtendGuestRequest, GatewayEvent,
  GuestExtendOffer, ListAdminGuestsQuery, ListAdminGuestsResponse, ListExpiringGuestsResponse, Permission,
  SpaceId, SpaceMemberResponse, SpaceRole, Target, UserId,
};
use time::{Duration, OffsetDateTime, Time, UtcOffset};
use tracing::warn;

use super::access::{SpaceAccess, allows, is_guest, load_space, require, space_target};
use super::audit::{self, Target as AuditTarget};
use super::convert::{offset_date_time, unix_millis, user_summary};
use super::spaces::remove_expired;
use super::workspace::{MAX_GUEST_EXPIRY_DAYS, current_guest_policy};
use crate::auth::AuthenticatedUser;
use crate::config::GuestsConfig;
use crate::error::ApiError;
use crate::meetings::ical::Method;
use crate::meetings::mailer::OutgoingMail;
use crate::runtime::brand_mail::run_per_route;
use crate::state::AppState;

/// Guests handled per run; a larger backlog continues at the next run.
const BATCH: i64 = 200;
/// Guests one person's "ending soon" list shows.
const EXPIRING_LIST_LIMIT: i64 = 100;
/// How far the notice offers to extend, in days.
const EXTEND_OFFER_DAYS: [u32; 2] = [30, 90];

/// Removes guests whose membership expired at `now` from their space and its channels, and tells the
/// guest, the sponsor and the space's admins. Returns how many were removed.
pub async fn expire_once(state: &AppState, now: OffsetDateTime) -> Result<usize, ApiError> {
  let expired = state.database.list_expired_guests(now, BATCH).await?;
  let mut removed = 0;
  for guest in expired {
    let Some(user) = state.database.get_user(guest.user).await? else {
      continue;
    };
    let members = state.database.list_space_members(guest.space).await?;
    let audience = terms_audience(&members, guest.user, guest.maybe_sponsor);

    match remove_expired(state, guest.space, guest.user, now).await {
      Ok(Some(_)) => {},
      // Extended or made a member since the list was read.
      Ok(None) => continue,
      Err(error) => {
        warn!(%error, space_id = %guest.space, "removing an expired guest failed");
        continue;
      },
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

/// What a caller with `right` can extend a guest to: days on top of the expiry, and where their limit
/// is nearer than that, the limit. The limit counts from the start of the day, so the list stays the
/// same all day and what it offers is still within the limit when it is taken.
fn extend_offers(right: ExtensionRight, expires_at: OffsetDateTime) -> Vec<GuestExtendOffer> {
  let maybe_limit = match right {
    ExtensionRight::None => return Vec::new(),
    ExtensionRight::Unlimited => None,
    ExtensionRight::UpTo(limit) => Some(limit.to_offset(UtcOffset::UTC).replace_time(Time::MIDNIGHT)),
  };
  let mut offers: Vec<GuestExtendOffer> = Vec::new();
  for days in EXTEND_OFFER_DAYS {
    let full = expires_at + Duration::days(i64::from(days));
    match maybe_limit {
      Some(limit) if limit <= expires_at => {},
      Some(limit) if limit < full => {
        if !offers.iter().any(|offer| offer.maybe_days.is_none()) {
          offers.push(GuestExtendOffer {
            until: unix_millis(limit),
            maybe_days: None,
          });
        }
      },
      _ => offers.push(GuestExtendOffer {
        until: unix_millis(full),
        maybe_days: Some(days),
      }),
    }
  }
  offers
}

/// Extends a guest: an absolute time, so that a retry changes nothing. Never shortens, and a removal,
/// a promotion or a later extension at the same moment stands (the database decides).
pub async fn extend(
  state: &AppState,
  caller: &AuthenticatedUser,
  space: SpaceId,
  user_id: UserId,
  request: ExtendGuestRequest,
) -> Result<SpaceMemberResponse, ApiError> {
  let access = load_space(state, caller, space).await?;
  let maybe_terms = state.database.space_member_terms(space, user_id).await?;
  let maybe_sponsor = maybe_terms.and_then(|(_, sponsor)| sponsor);
  let now = OffsetDateTime::now_utc();
  let right = right_of(
    state,
    caller,
    &access.target(),
    maybe_sponsor,
    access.view.maybe_role.is_some(),
    now,
  );
  // First: somebody who may not extend learns nothing about who is a guest here.
  if right == ExtensionRight::None {
    return Err(ApiError::Forbidden("you may not extend this guest".into()));
  }
  if state.database.space_role(space, user_id).await? != Some(SpaceRole::Guest) {
    return Err(ApiError::user_not_found());
  }
  let user = state
    .database
    .get_user(user_id)
    .await?
    .ok_or_else(ApiError::user_not_found)?;
  let at = offset_date_time(request.expires_at)
    .filter(|at| *at > now && *at <= now + Duration::days(i64::from(MAX_GUEST_EXPIRY_DAYS)))
    .ok_or_else(|| ApiError::InvalidRequest("expires_at must be in the future, within ten years".into()))?;
  let Some((Some(current), _)) = maybe_terms else {
    return Err(ApiError::Conflict("this guest does not expire".into()));
  };
  if at < current {
    return Err(ApiError::InvalidRequest(
      "extending can't shorten the membership".into(),
    ));
  }
  ensure_within(right, at)?;

  let expires_at = if state.database.extend_guest_expiry(space, user_id, at).await? {
    audit::record(
      state,
      Some(caller.user.id),
      "guest_extended",
      Some(AuditTarget::new("space", space)),
      &json!({"user_id": user_id.to_string(), "expires_at": unix_millis(at).0}),
    )
    .await?;
    publish_terms(state, space, user_id, maybe_sponsor).await?;
    at
  } else {
    // A retry, somebody who extended further, or it changed under this request: what stands now.
    if state.database.space_role(space, user_id).await? != Some(SpaceRole::Guest) {
      return Err(ApiError::user_not_found());
    }
    state
      .database
      .space_member_terms(space, user_id)
      .await?
      .and_then(|(expires, _)| expires)
      .ok_or_else(ApiError::user_not_found)?
  };
  Ok(SpaceMemberResponse {
    user: user_summary(state, &user),
    role: SpaceRole::Guest,
    maybe_expires_at: Some(unix_millis(expires_at)),
    maybe_sponsor_user_id: maybe_sponsor,
  })
}

/// The sponsor's limit also holds for a new membership with an expiry of the caller's choice, or
/// removing and adding again would go around it. The caller becomes the sponsor.
pub(super) fn check_new_expiry(
  state: &AppState,
  caller: &AuthenticatedUser,
  target: &Target,
  at: OffsetDateTime,
) -> Result<(), ApiError> {
  ensure_within(
    extension_right(
      is_guest(caller),
      allows(state, caller, &Permission::ManageSpaceMembers, target),
      true,
      current_guest_policy(state).effective_sponsor_extension_days(),
      OffsetDateTime::now_utc(),
    ),
    at,
  )
}

/// A role or expiry changed through the general member update: a new expiry is audited, and those
/// concerned read their list again (a demoted admin's list shrinks, a promoted guest's notice goes).
pub(super) async fn note_terms_change(
  state: &AppState,
  caller: &AuthenticatedUser,
  space: SpaceId,
  user_id: UserId,
  change: ExpiryChange,
) -> Result<(), ApiError> {
  let maybe_expires_at = match change {
    ExpiryChange::Keep => return publish_member_terms(state, space, user_id).await,
    ExpiryChange::Set(at) => Some(unix_millis(at).0),
    ExpiryChange::Clear => None,
  };
  audit::record(
    state,
    Some(caller.user.id),
    "guest_extended",
    Some(AuditTarget::new("space", space)),
    &json!({"user_id": user_id.to_string(), "expires_at": maybe_expires_at}),
  )
  .await?;
  publish_member_terms(state, space, user_id).await
}

async fn publish_member_terms(state: &AppState, space: SpaceId, user_id: UserId) -> Result<(), ApiError> {
  let maybe_sponsor = state
    .database
    .space_member_terms(space, user_id)
    .await?
    .and_then(|(_, sponsor)| sponsor);
  publish_terms(state, space, user_id, maybe_sponsor).await
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
        extend_offers: extend_offers(right, row.expires_at),
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
      maybe_sponsor_query: query.maybe_sponsor_query,
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

/// Sends the notices due at `now`: one event for everybody concerned, whose apps read the list
/// again, and one mail per person. The mails name no guest and no space. A lost mail is not sent
/// again: the notice in the app stands until the expiry. A guest whose notice fails, or whose expiry
/// changed meanwhile, doesn't hold up the others. Returns how many memberships were noticed.
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
  let mut audience: HashSet<UserId> = HashSet::new();
  let mut members: HashMap<SpaceId, Vec<SpaceMember>> = HashMap::new();
  let mut sent = 0;
  for notice in &due {
    match note_notice(state, notice, &mut members, &mut audience).await {
      Ok(true) => {},
      Ok(false) => continue,
      Err(error) => {
        warn!(%error, space_id = %notice.space, "a guest expiry notice failed");
        continue;
      },
    }
    sent += 1;
    if let Some(sponsor) = notice.maybe_sponsor {
      *sponsored.entry(sponsor).or_default() += 1;
    }
    let days = days_left(notice, now);
    guests
      .entry(notice.user)
      .and_modify(|left| *left = days.min(*left))
      .or_insert(days);
  }
  if !audience.is_empty() {
    state.hub.publish(audience, &GatewayEvent::GuestTermsUpdate);
  }
  mail_notices(state, &sponsored, &guests).await?;
  Ok(sent)
}

/// Records one notice and adds whom it concerns; `false` when the expiry moved meanwhile and nothing
/// is to be said. The space's members are read once per run.
async fn note_notice(
  state: &AppState,
  notice: &GuestNoticeDue,
  members: &mut HashMap<SpaceId, Vec<SpaceMember>>,
  audience: &mut HashSet<UserId>,
) -> Result<bool, ApiError> {
  if let Entry::Vacant(slot) = members.entry(notice.space) {
    slot.insert(state.database.list_space_members(notice.space).await?);
  }
  if !state.database.record_guest_notice(notice).await? {
    return Ok(false);
  }
  if let Some(listed) = members.get(&notice.space) {
    audience.extend(terms_audience(listed, notice.user, notice.maybe_sponsor));
  }
  Ok(true)
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
  let people: Vec<UserId> = sponsored.keys().chain(guests.keys()).copied().collect();
  let emails = state.database.get_user_emails(&people).await?;
  let settings = state.settings.get();
  let config = &settings.guests;
  let brands = state.brands.current();
  let mut outgoing = Vec::new();
  let mut queued = Vec::new();
  for (sponsor, count) in sponsored {
    outgoing.push((
      *sponsor,
      &config.notice_subject_sponsor,
      sponsor_body(config, *count),
      count.to_string(),
      String::new(),
    ));
  }
  for (guest, days) in guests {
    outgoing.push((
      *guest,
      &config.notice_subject_guest,
      guest_body(config, *days),
      String::new(),
      days.to_string(),
    ));
  }
  for (user, subject, body, count, days) in outgoing {
    let Some(address) = emails.get(&user) else {
      continue;
    };
    let brand = brands.for_email(address);
    let Some(mailer) = state.mailer_for(brand) else {
      continue;
    };
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
    queued.push((state.mail_route(brand), (user, mailer, mail)));
  }
  run_per_route(queued, |queue| async move {
    for (user, mailer, mail) in queue {
      if let Err(error) = mailer.send(&mail).await {
        warn!(%error, user_id = %user, "mailing a guest expiry notice failed");
      }
    }
  })
  .await;
  Ok(())
}

fn sponsor_body(config: &GuestsConfig, count: u32) -> &String {
  if count == 1 {
    &config.notice_body_sponsor_one
  } else {
    &config.notice_body_sponsor
  }
}

fn guest_body(config: &GuestsConfig, days: i32) -> &String {
  if days == 1 {
    &config.notice_body_guest_one
  } else {
    &config.notice_body_guest
  }
}

/// Whom a guest's terms concern: the guest, the space's admins and the sponsor if still in the space
/// (a private space tells nothing to somebody who was removed from it).
pub(super) fn terms_audience(
  members: &[SpaceMember],
  guest: UserId,
  maybe_sponsor: Option<UserId>,
) -> Vec<UserId> {
  members
    .iter()
    .filter(|member| member.role == SpaceRole::SpaceAdmin || Some(member.user.id) == maybe_sponsor)
    .map(|member| member.user.id)
    .chain([guest])
    .collect()
}

/// Tells those concerned to read the list again.
async fn publish_terms(
  state: &AppState,
  space: SpaceId,
  user: UserId,
  maybe_sponsor: Option<UserId>,
) -> Result<(), ApiError> {
  let members = state.database.list_space_members(space).await?;
  state.hub.publish(
    terms_audience(&members, user, maybe_sponsor),
    &GatewayEvent::GuestTermsUpdate,
  );
  Ok(())
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

  mod offers {
    use super::*;

    const EXPIRES: OffsetDateTime = time::macros::datetime!(2026-11-10 12:00 UTC);

    fn days_and_until(right: ExtensionRight) -> Vec<(Option<u32>, OffsetDateTime)> {
      extend_offers(right, EXPIRES)
        .into_iter()
        .map(|offer| (offer.maybe_days, offset_date_time(offer.until).unwrap()))
        .collect()
    }

    #[test]
    fn nobody_without_the_right_gets_any() {
      assert!(days_and_until(ExtensionRight::None).is_empty());
    }

    #[test]
    fn admins_and_sponsors_with_room_get_both() {
      let both = vec![
        (Some(30), EXPIRES + Duration::days(30)),
        (Some(90), EXPIRES + Duration::days(90)),
      ];
      assert_eq!(days_and_until(ExtensionRight::Unlimited), both);
      assert_eq!(
        days_and_until(ExtensionRight::UpTo(NOW + Duration::days(120))),
        both
      );
    }

    /// The "up to 30 days" preset: the button stays, as far as the limit goes, and is accepted.
    #[test]
    fn a_nearer_limit_is_offered_as_itself_once() {
      let right = ExtensionRight::UpTo(NOW + Duration::days(30));
      let midnight = time::macros::datetime!(2026-12-05 0:00 UTC);
      assert_eq!(days_and_until(right), vec![(None, midnight)]);
      assert!(ensure_within(right, midnight).is_ok());
    }

    #[test]
    fn a_limit_before_the_expiry_offers_nothing() {
      assert!(days_and_until(ExtensionRight::UpTo(NOW + Duration::days(1))).is_empty());
    }

    #[test]
    fn they_stay_the_same_through_the_day() {
      let morning = ExtensionRight::UpTo(time::macros::datetime!(2026-12-05 8:00 UTC));
      let evening = ExtensionRight::UpTo(time::macros::datetime!(2026-12-05 22:00 UTC));
      assert_eq!(extend_offers(morning, EXPIRES), extend_offers(evening, EXPIRES));
    }
  }

  #[test]
  fn the_mail_says_one_where_it_is_one() {
    let config = GuestsConfig::default();
    assert_eq!(sponsor_body(&config, 1), &config.notice_body_sponsor_one);
    assert_eq!(sponsor_body(&config, 2), &config.notice_body_sponsor);
    assert_eq!(guest_body(&config, 1), &config.notice_body_guest_one);
    assert_eq!(guest_body(&config, 3), &config.notice_body_guest);
    assert!(!config.notice_body_sponsor_one.contains("{count}"));
    assert!(!config.notice_body_guest_one.contains("{days}"));
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
