//! Share links for external download (e2ee.md §4.5.1). A member makes a link to a file they can
//! read; anybody with the link downloads the ciphertext (the key is in the URL fragment and never
//! reaches the server). The server enforces expiry, the download limit and revocation.

use rand_core::OsRng;
use serde_json::json;
use teamcraft_crypto::account::new_one_time_token;
use teamcraft_db::{NewShareLink, ShareLink};
use teamcraft_proto::{
  CreateShareLinkRequest, ListShareLinksResponse, Permission, ShareDownloadResponse, ShareInfoResponse,
  ShareLinkResponse,
};
use time::{Duration, OffsetDateTime};

use super::access::{is_instance_admin, require};
use super::attachments::readable_attachment;
use super::audit::{self, Target as AuditTarget};
use super::convert::{offset_date_time, unix_millis};
use crate::auth::AuthenticatedUser;
use crate::brand::ResolvedBrand;
use crate::error::ApiError;
use crate::state::AppState;
use crate::storage::BlobStore;

/// Links shown in the caller's list.
const LIST_LIMIT: i64 = 200;

/// Makes a link to a file the caller can read and may share (`create_share_link`).
pub async fn create_share(
  state: &AppState,
  caller: &AuthenticatedUser,
  brand: &ResolvedBrand,
  request: CreateShareLinkRequest,
) -> Result<ShareLinkResponse, ApiError> {
  let settings = &state.settings.get().shares;
  if !settings.is_enabled {
    return Err(ApiError::Forbidden("share links are switched off".into()));
  }
  let (attachment, access) = readable_attachment(state, caller, request.attachment_id).await?;
  require(state, caller, Permission::CreateShareLink, &access.target())?;

  let now = OffsetDateTime::now_utc();
  let latest = now + Duration::days(i64::from(settings.max_expiry_days));
  if request.maybe_expires_at.is_some() && request.maybe_expires_in_seconds.is_some() {
    return Err(ApiError::InvalidRequest(
      "send expires_at or expires_in_seconds, not both".into(),
    ));
  }
  let expires_at = match (request.maybe_expires_at, request.maybe_expires_in_seconds) {
    (None, Some(0)) => {
      return Err(ApiError::InvalidRequest(
        "expires_in_seconds must be positive".into(),
      ));
    },
    // A relative choice is measured by the server's clock and shortened to the cap.
    (None, Some(seconds)) => {
      let seconds = i64::try_from(seconds).unwrap_or(i64::MAX);
      now
        .checked_add(Duration::seconds(seconds))
        .map_or(latest, |at| at.min(latest))
    },
    (Some(millis), _) => {
      let at =
        offset_date_time(millis).ok_or_else(|| ApiError::InvalidRequest("invalid expires_at".into()))?;
      if at <= now {
        return Err(ApiError::InvalidRequest(
          "expires_at must be in the future".into(),
        ));
      }
      if at > latest {
        return Err(ApiError::InvalidRequest(format!(
          "links last at most {} days",
          settings.max_expiry_days
        )));
      }
      at
    },
    (None, None) => (now + Duration::days(i64::from(settings.default_expiry_days))).min(latest),
  };
  let cap = settings.max_downloads_cap;
  let maybe_max_downloads = match request.maybe_max_downloads {
    Some(0) => return Err(ApiError::InvalidRequest("max_downloads must be positive".into())),
    Some(max) if cap > 0 && max > cap => {
      return Err(ApiError::InvalidRequest(format!(
        "links allow at most {cap} downloads"
      )));
    },
    Some(max) => Some(max),
    None => (cap > 0).then_some(cap),
  };
  let live = state
    .database
    .list_share_links_of(caller.user.id, i64::MAX)
    .await?
    .iter()
    .filter(|link| !link.is_revoked && link.expires_at > now)
    .count();
  if live >= settings.max_links_per_user {
    return Err(ApiError::Conflict("revoke some links first".into()));
  }

  let id = new_one_time_token(&mut OsRng);
  let link = state
    .database
    .create_share_link(&NewShareLink {
      id: &id,
      attachment: attachment.id,
      created_by: caller.user.id,
      expires_at,
      maybe_max_downloads,
      is_password_protected: request.is_password_protected,
      maybe_brand_id: Some(&brand.brand.id),
    })
    .await?;
  // The link ID is a secret of sorts: the audit entry names the attachment, not the link.
  audit::record(
    state,
    Some(caller.user.id),
    "share_link_created",
    Some(AuditTarget::new("attachment", attachment.id)),
    &json!({"expires_at": unix_millis(expires_at).0, "max_downloads": maybe_max_downloads}),
  )
  .await?;
  Ok(response(state, &link))
}

/// The caller's links, newest first.
pub async fn list_shares(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<ListShareLinksResponse, ApiError> {
  let links = state
    .database
    .list_share_links_of(caller.user.id, LIST_LIMIT)
    .await?;
  Ok(ListShareLinksResponse {
    links: links.iter().map(|link| response(state, link)).collect(),
  })
}

/// Stops a link from working. Its creator, or an instance admin.
pub async fn revoke_share(state: &AppState, caller: &AuthenticatedUser, id: &str) -> Result<(), ApiError> {
  let link = state
    .database
    .get_share_link(id)
    .await?
    .filter(|link| link.created_by == caller.user.id || is_instance_admin(caller))
    .ok_or_else(ApiError::attachment_not_found)?;
  if !state
    .database
    .revoke_share_link(id, caller.user.id, is_instance_admin(caller))
    .await?
  {
    return Err(ApiError::Conflict("the link was revoked already".into()));
  }
  audit::record(
    state,
    Some(caller.user.id),
    "share_link_revoked",
    Some(AuditTarget::new("attachment", link.attachment)),
    &json!({}),
  )
  .await?;
  Ok(())
}

/// Public: what the download page shows before it asks for the file. Unknown, revoked, expired and
/// used-up links all answer the same.
pub async fn share_info(
  state: &AppState,
  brand: &ResolvedBrand,
  id: &str,
) -> Result<ShareInfoResponse, ApiError> {
  let (link, target) = usable_share(state, brand, id).await?;
  Ok(ShareInfoResponse {
    size_bytes: target.size_bytes,
    is_password_protected: link.is_password_protected,
    expires_at: unix_millis(link.expires_at),
  })
}

/// Public: counts one download and returns a short-lived URL of the ciphertext, with a grant.
/// A retry that presents the grant of a download counted within the retry window gets a fresh URL
/// without counting again, so a transient failure on the way (the storage fetch, the connection)
/// does not use up a link with a download limit. Only the grant's hash is stored.
pub async fn download_share(
  state: &AppState,
  brand: &ResolvedBrand,
  id: &str,
  maybe_grant: Option<&str>,
) -> Result<ShareDownloadResponse, ApiError> {
  let store = state
    .maybe_blob_store
    .as_ref()
    .ok_or_else(|| ApiError::Unavailable("storage is not configured".into()))?;
  let now = OffsetDateTime::now_utc();
  let retry_window = Duration::minutes(i64::from(state.settings.get().shares.download_retry_minutes));
  if let Some(presented) = maybe_grant.filter(|_| !retry_window.is_zero())
    && let Some((link, target)) = state
      .database
      .find_share_by_grant(id, &grant_hash(presented), now)
      .await?
    && state.settings.get().shares.is_enabled
    && is_served_on(state, &link, brand)
  {
    return Ok(download_response(store, &target, presented.to_owned()));
  }
  // No grant, or one that ran out: a new download.
  let grant = new_one_time_token(&mut OsRng);
  let (_, target) = usable_share(state, brand, id).await?;
  if !state
    .database
    .count_share_download(id, now, &grant_hash(&grant), now + retry_window)
    .await?
  {
    return Err(ApiError::ShareUnavailable);
  }
  Ok(download_response(store, &target, grant))
}

fn download_response(
  store: &BlobStore,
  target: &teamcraft_db::ShareTarget,
  grant: String,
) -> ShareDownloadResponse {
  ShareDownloadResponse {
    grant,
    download_url: store.presign_download(&target.s3_key).to_string(),
    size_bytes: target.size_bytes,
    expires_at: unix_millis(OffsetDateTime::now_utc() + store.expiry()),
  }
}

fn grant_hash(grant: &str) -> [u8; 32] {
  teamcraft_crypto::attachment::content_sha256(grant.as_bytes())
}

async fn usable_share(
  state: &AppState,
  brand: &ResolvedBrand,
  id: &str,
) -> Result<(ShareLink, teamcraft_db::ShareTarget), ApiError> {
  if !state.settings.get().shares.is_enabled {
    return Err(ApiError::ShareUnavailable);
  }
  let (link, target) = state
    .database
    .find_downloadable_share(id, OffsetDateTime::now_utc())
    .await?
    .ok_or(ApiError::ShareUnavailable)?;
  if !is_served_on(state, &link, brand) {
    return Err(ApiError::ShareUnavailable);
  }
  Ok((link, target))
}

/// A link works only on the host names of the brand that made it (docs/branding.md §8), so that a
/// file is never presented under another brand's name. Links of a brand that is gone from the
/// configuration belong to the default brand; links from before brands existed work everywhere.
fn is_served_on(state: &AppState, link: &ShareLink, request: &ResolvedBrand) -> bool {
  let Some(stored) = link.maybe_brand_id.as_deref() else {
    return true;
  };
  let brands = state.brands.current();
  let owner = brands.by_id(stored).unwrap_or(brands.default_brand());
  request.brand.id == owner.id && (owner.hostnames.is_empty() || request.is_known_host)
}

fn response(state: &AppState, link: &ShareLink) -> ShareLinkResponse {
  // The link names a host of the brand that made it; without brand hosts the configured base is used.
  let brands = state.brands.current();
  let brand = link
    .maybe_brand_id
    .as_deref()
    .and_then(|id| brands.by_id(id))
    .unwrap_or(brands.default_brand());
  let brand_origin = brand.origin();
  let settings = state.settings.get();
  let configured = settings.shares.public_base_url.trim_end_matches('/');
  let base = if brand_origin.is_empty() {
    configured
  } else {
    brand_origin.as_str()
  };
  ShareLinkResponse {
    id: link.id.clone(),
    attachment_id: link.attachment,
    maybe_url: (!base.is_empty()).then(|| format!("{base}/f/{}", link.id)),
    created_at: unix_millis(link.created_at),
    expires_at: unix_millis(link.expires_at),
    maybe_max_downloads: link.maybe_max_downloads,
    is_password_protected: link.is_password_protected,
    download_count: link.download_count,
    maybe_last_downloaded_at: link.maybe_last_downloaded_at.map(unix_millis),
    is_revoked: link.is_revoked,
  }
}
