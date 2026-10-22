//! Guest lifecycle (PLAN.md §12): guests expire, and their sponsor hears about it.

use serde_json::json;
use teamcraft_proto::{GatewayEvent, SpaceRole};
use time::OffsetDateTime;
use tracing::warn;

use super::audit::{self, Target as AuditTarget};
use super::convert::user_summary;
use super::spaces::remove_everywhere;
use crate::error::ApiError;
use crate::state::AppState;

/// Guests handled per run; a larger backlog continues at the next run.
const BATCH: i64 = 200;

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

    if let Err(error) = remove_everywhere(state, guest.space, guest.user).await {
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
