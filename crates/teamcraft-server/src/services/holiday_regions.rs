//! An organizer's own holiday region (#1441, docs/meetings.md, "Holidays"): whose public holidays
//! their series skip, for people in another state or country than the instance's region. The
//! instance's holiday feed counts on top of either region.
//!
//! A setting of its own (`/me/holiday-region`), not a field of the profile: the profile's update
//! replaces every field, so apps that don't know this one would clear it with every change of the
//! name. And the profile is what others see; this isn't.

use teamcraft_proto::{HolidayRegionResponse, SetHolidayRegionRequest};

use super::access::is_guest;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::holidays::HolidayRegion;
use crate::state::AppState;

/// The caller's region and what applies without one.
pub async fn get_holiday_region(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<HolidayRegionResponse, ApiError> {
  require_member(caller)?;
  respond(state, caller).await
}

/// Sets the caller's region (absent or empty: the instance's). A change writes the calendar
/// events of the caller's series that skip holidays again, in the background.
pub async fn put_holiday_region(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: SetHolidayRegionRequest,
) -> Result<HolidayRegionResponse, ApiError> {
  require_member(caller)?;
  let maybe_region = request
    .maybe_region
    .map(|region| region.trim().to_owned())
    .filter(|region| !region.is_empty());
  if maybe_region
    .as_deref()
    .is_some_and(|region| HolidayRegion::from_code(region).is_none())
  {
    return Err(ApiError::InvalidRequest(
      "region must be a region of the holiday table, like DE-BY or CH-ZH".into(),
    ));
  }
  let is_changed = state
    .database
    .set_holiday_region(caller.user.id, maybe_region.as_deref())
    .await?;
  if is_changed {
    state.holiday_resync.region_changed(caller.user.id);
  }
  respond(state, caller).await
}

async fn respond(state: &AppState, caller: &AuthenticatedUser) -> Result<HolidayRegionResponse, ApiError> {
  let settings = state.settings.get().holidays.clone();
  Ok(HolidayRegionResponse {
    maybe_region: state.database.holiday_region(caller.user.id).await?,
    instance_region: settings.region,
    has_instance_feed: settings.maybe_feed_url.is_some(),
  })
}

/// Guests don't organize meetings.
fn require_member(caller: &AuthenticatedUser) -> Result<(), ApiError> {
  if is_guest(caller) {
    return Err(ApiError::Forbidden("guests have no holiday region".into()));
  }
  Ok(())
}
