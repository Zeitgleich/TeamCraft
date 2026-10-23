//! Brands in the admin console (docs/branding.md §2.1): list, create, edit, delete and take over
//! brands, and their images. Configuration brands are read-only until an admin takes one over into
//! the database; images of database brands live in the object storage. Every change builds and
//! checks the complete registry first and only then stores it, so a change that would leave two
//! defaults or a host name on two brands is refused as a whole.

use std::collections::{BTreeMap, BTreeSet};

use axum::body::Bytes;
use serde_json::json;
use teamcraft_db::StoredBrandAsset;
use teamcraft_proto::{
  AdminBrand, AdminBrandsResponse, BrandAssetSlot, BrandColors, BrandLinks, BrandMailSettings, BrandSettings,
  BrandSource, CreateBrandRequest, Permission, ServedHostname, Target, UpdateBrandRequest,
};
use time::OffsetDateTime;
use tracing::{error, warn};

use super::access::require;
use super::audit;
use super::convert::unix_millis;
use crate::auth::AuthenticatedUser;
use crate::brand::{Brand, BrandAsset, BrandError, DatabaseBrand, StoredAsset, normalize_host, sniff_image};
use crate::config::{BrandColorsConfig, BrandConfig};
use crate::error::ApiError;
use crate::state::AppState;
use crate::storage::BlobStore;

/// Puts the database brands in force on top of the configuration's; at startup. A stored brand that
/// no longer fits (a host name the server stopped serving) loses that host name with a warning; if
/// the brands still don't fit together, the configuration's brands alone stay in force.
pub async fn load(state: &AppState) -> Result<(), ApiError> {
  let mut database = read_database_brands(state).await?;
  let served: BTreeSet<String> = state
    .brands
    .current()
    .served_hostnames()
    .into_iter()
    .map(|(host, _)| host)
    .collect();
  for brand in &mut database {
    brand.config.hostnames.retain(|host| {
      let is_served = served.contains(host);
      if !is_served {
        warn!(brand_id = %brand.config.id, %host, "the server no longer serves this host name; the brand loses it");
      }
      is_served
    });
  }
  match state.brands.check(&database) {
    Ok(registry) => state.brands.install(registry, database),
    Err(error) => {
      error!(%error, "the brands made in the admin console are invalid; only the configured brands are served")
    },
  }
  Ok(())
}

pub async fn list_brands(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<AdminBrandsResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  Ok(response(state))
}

/// Makes a new database brand.
pub async fn create_brand(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: CreateBrandRequest,
) -> Result<AdminBrandsResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  let id = request.id.trim().to_owned();
  {
    let _guard = state.settings_lock.lock().await;
    if state.brands.current().by_id(&id).is_some() {
      return Err(ApiError::Conflict(format!("a brand {id:?} exists already")));
    }
    let config = apply_settings(BrandConfig::default(), &id, request.is_default, request.settings);
    let mut database = state.brands.database_brands();
    clear_other_defaults(&mut database, &id, config.is_default);
    database.push(DatabaseBrand {
      config: config.clone(),
      updated_at: OffsetDateTime::now_utc(),
      assets: BTreeMap::new(),
    });
    let registry = state.brands.check(&database).map_err(invalid)?;
    store_brand(state, caller, &config).await?;
    state.brands.install(registry, database);
  }
  audit::record(
    state,
    Some(caller.user.id),
    "brand_created",
    None,
    &json!({"brand_id": id}),
  )
  .await?;
  Ok(response(state))
}

/// Replaces the settings of a database brand; its images stay.
pub async fn update_brand(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: &str,
  request: UpdateBrandRequest,
) -> Result<AdminBrandsResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  {
    let _guard = state.settings_lock.lock().await;
    let mut database = state.brands.database_brands();
    let index = database_index(state, &database, id)?;
    let config = apply_settings(
      database[index].config.clone(),
      id,
      request.is_default,
      request.settings,
    );
    database[index].config = config.clone();
    database[index].updated_at = OffsetDateTime::now_utc();
    clear_other_defaults(&mut database, id, config.is_default);
    let registry = state.brands.check(&database).map_err(invalid)?;
    store_brand(state, caller, &config).await?;
    state.brands.install(registry, database);
  }
  audit::record(
    state,
    Some(caller.user.id),
    "brand_updated",
    None,
    &json!({"brand_id": id}),
  )
  .await?;
  Ok(response(state))
}

/// Deletes a database brand and its images; a configuration brand with the same id comes back.
pub async fn delete_brand(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: &str,
) -> Result<AdminBrandsResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  {
    let _guard = state.settings_lock.lock().await;
    let mut database = state.brands.database_brands();
    let index = database_index(state, &database, id)?;
    database.remove(index);
    let registry = state.brands.check(&database).map_err(invalid)?;
    let keys = state.database.delete_brand(id).await?.unwrap_or_default();
    state.brands.install(registry, database);
    delete_objects(state, &keys).await;
  }
  audit::record(
    state,
    Some(caller.user.id),
    "brand_deleted",
    None,
    &json!({"brand_id": id}),
  )
  .await?;
  Ok(response(state))
}

/// Copies a configuration brand, with its images, into the database, where it can be edited. The
/// configuration file's version stays as the fallback.
pub async fn take_over_brand(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: &str,
) -> Result<AdminBrandsResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  {
    let _guard = state.settings_lock.lock().await;
    let registry = state.brands.current();
    let brand = registry.by_id(id).ok_or_else(brand_not_found)?;
    if brand.source != BrandSource::Config {
      return Err(ApiError::Conflict(
        "only brands of the configuration file can be taken over".into(),
      ));
    }
    let slots = brand.asset_slots();
    let store = if slots.is_empty() {
      None
    } else {
      Some(blob_store(state)?)
    };
    let mut config = brand.config.clone();
    clear_file_names(&mut config);
    let mut assets = BTreeMap::new();
    for slot in &slots {
      if let Some(asset) = brand.asset(slot) {
        assets.insert(
          slot.as_str().to_owned(),
          StoredAsset {
            object_key: object_key(state, id, slot)?,
            content_type: asset.content_type,
            bytes: asset.bytes.clone(),
          },
        );
      }
    }
    let mut database = state.brands.database_brands();
    database.push(DatabaseBrand {
      config: config.clone(),
      updated_at: OffsetDateTime::now_utc(),
      assets: assets.clone(),
    });
    let candidate = state.brands.check(&database).map_err(invalid)?;
    if let Some(store) = store {
      for asset in assets.values() {
        store
          .put_object(&asset.object_key, asset.bytes.to_vec(), asset.content_type)
          .await
          .map_err(|error| ApiError::Unavailable(format!("storing a brand image failed: {error}")))?;
      }
    }
    store_brand(state, caller, &config).await?;
    for (slot, asset) in &assets {
      state
        .database
        .put_brand_asset(&asset_row(id, slot, asset))
        .await?;
    }
    state.brands.install(candidate, database);
  }
  audit::record(
    state,
    Some(caller.user.id),
    "brand_taken_over",
    None,
    &json!({"brand_id": id}),
  )
  .await?;
  Ok(response(state))
}

/// One image of any brand, for the console's preview.
pub async fn get_asset(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: &str,
  slot: &BrandAssetSlot,
) -> Result<BrandAsset, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  let registry = state.brands.current();
  let brand = registry.by_id(id).ok_or_else(brand_not_found)?;
  brand
    .asset(slot)
    .cloned()
    .ok_or_else(ApiError::brand_asset_not_found)
}

/// Sets the image of a slot of a database brand.
pub async fn put_asset(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: &str,
  slot: &BrandAssetSlot,
  declared_type: &str,
  bytes: Bytes,
) -> Result<AdminBrandsResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  if matches!(slot, BrandAssetSlot::Unknown(_)) {
    return Err(ApiError::brand_asset_not_found());
  }
  let max_bytes = state.brands.inputs().branding.max_asset_bytes;
  if bytes.is_empty() || bytes.len() as u64 > max_bytes {
    return Err(ApiError::PayloadTooLarge(format!(
      "brand images are 1 to {max_bytes} bytes"
    )));
  }
  let content_type = sniff_image(&bytes, declared_type).ok_or_else(|| {
    ApiError::InvalidRequest("brand images are PNG, JPEG, WebP, ICO or SVG files of the declared type".into())
  })?;
  {
    let _guard = state.settings_lock.lock().await;
    let mut database = state.brands.database_brands();
    let index = database_index(state, &database, id)?;
    let store = blob_store(state)?;
    let asset = StoredAsset {
      object_key: object_key(state, id, slot)?,
      content_type,
      bytes,
    };
    database[index]
      .assets
      .insert(slot.as_str().to_owned(), asset.clone());
    database[index].updated_at = OffsetDateTime::now_utc();
    let registry = state.brands.check(&database).map_err(invalid)?;
    store
      .put_object(&asset.object_key, asset.bytes.to_vec(), content_type)
      .await
      .map_err(|error| ApiError::Unavailable(format!("storing the image failed: {error}")))?;
    let maybe_previous = state
      .database
      .put_brand_asset(&asset_row(id, slot.as_str(), &asset))
      .await?;
    state.brands.install(registry, database);
    if let Some(previous) = maybe_previous {
      delete_objects(state, &[previous]).await;
    }
  }
  audit::record(
    state,
    Some(caller.user.id),
    "brand_asset_set",
    None,
    &json!({"brand_id": id, "slot": slot.as_str()}),
  )
  .await?;
  Ok(response(state))
}

/// Removes the image of a slot of a database brand.
pub async fn delete_asset(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: &str,
  slot: &BrandAssetSlot,
) -> Result<AdminBrandsResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  {
    let _guard = state.settings_lock.lock().await;
    let mut database = state.brands.database_brands();
    let index = database_index(state, &database, id)?;
    if database[index].assets.remove(slot.as_str()).is_none() {
      return Err(ApiError::brand_asset_not_found());
    }
    database[index].updated_at = OffsetDateTime::now_utc();
    let registry = state.brands.check(&database).map_err(invalid)?;
    let maybe_key = state.database.delete_brand_asset(id, slot.as_str()).await?;
    state.brands.install(registry, database);
    if let Some(key) = maybe_key {
      delete_objects(state, &[key]).await;
    }
  }
  audit::record(
    state,
    Some(caller.user.id),
    "brand_asset_removed",
    None,
    &json!({"brand_id": id, "slot": slot.as_str()}),
  )
  .await?;
  Ok(response(state))
}

fn response(state: &AppState) -> AdminBrandsResponse {
  let registry = state.brands.current();
  AdminBrandsResponse {
    brands: registry
      .brands()
      .into_iter()
      .map(|brand| admin_brand(brand))
      .collect(),
    served_hostnames: registry
      .served_hostnames()
      .into_iter()
      .map(|(hostname, maybe_brand_id)| ServedHostname {
        hostname,
        maybe_brand_id,
      })
      .collect(),
    is_image_upload_available: state.maybe_blob_store.is_some(),
    max_asset_bytes: state.brands.inputs().branding.max_asset_bytes,
  }
}

fn admin_brand(brand: &Brand) -> AdminBrand {
  AdminBrand {
    id: brand.id.clone(),
    source: brand.source.clone(),
    is_default: brand.is_default,
    is_overriding_config: brand.is_overriding_config,
    settings: settings_of(&brand.config),
    assets: brand.asset_slots(),
    maybe_updated_at: brand.maybe_updated_at.map(unix_millis),
  }
}

/// The database brands with their image bytes from the object storage. An image that cannot be
/// read is left out (with a warning) rather than keeping the server from starting.
async fn read_database_brands(state: &AppState) -> Result<Vec<DatabaseBrand>, ApiError> {
  let rows = state.database.stored_brands().await?;
  let asset_rows = state.database.stored_brand_assets().await?;
  let max_bytes = state.brands.inputs().branding.max_asset_bytes;
  let mut brands = Vec::new();
  for row in rows {
    let mut config: BrandConfig = match serde_json::from_str(&row.settings) {
      Ok(config) => config,
      Err(error) => {
        error!(brand_id = %row.id, %error, "a stored brand cannot be read; it is skipped");
        continue;
      },
    };
    config.id.clone_from(&row.id);
    config.is_default = row.is_default;
    clear_file_names(&mut config);
    let mut assets = BTreeMap::new();
    for asset in asset_rows.iter().filter(|asset| asset.brand_id == row.id) {
      if let Some(stored) = read_asset(state, asset, max_bytes).await {
        assets.insert(asset.slot.clone(), stored);
      }
    }
    brands.push(DatabaseBrand {
      config,
      updated_at: row.updated_at,
      assets,
    });
  }
  Ok(brands)
}

async fn read_asset(state: &AppState, row: &StoredBrandAsset, max_bytes: u64) -> Option<StoredAsset> {
  let Some(store) = &state.maybe_blob_store else {
    warn!(brand_id = %row.brand_id, slot = %row.slot, "no object storage configured; a brand image is left out");
    return None;
  };
  let content_type = known_content_type(&row.content_type)?;
  match store.get_object(&row.object_key, max_bytes).await {
    Ok(Some(bytes)) => Some(StoredAsset {
      object_key: row.object_key.clone(),
      content_type,
      bytes: Bytes::from(bytes),
    }),
    Ok(None) => {
      warn!(brand_id = %row.brand_id, slot = %row.slot, "a brand image is missing in the object storage");
      None
    },
    Err(error) => {
      warn!(brand_id = %row.brand_id, slot = %row.slot, %error, "a brand image cannot be read");
      None
    },
  }
}

/// The static content type for a stored one; unknown types are not served.
fn known_content_type(stored: &str) -> Option<&'static str> {
  [
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/x-icon",
    "image/svg+xml",
  ]
  .into_iter()
  .find(|known| *known == stored)
}

async fn store_brand(
  state: &AppState,
  caller: &AuthenticatedUser,
  config: &BrandConfig,
) -> Result<(), ApiError> {
  let settings = serde_json::to_string(config).map_err(|error| ApiError::Internal(error.to_string()))?;
  state
    .database
    .put_brand(&config.id, config.is_default, &settings, caller.user.id)
    .await?;
  Ok(())
}

/// Where a database brand is in the list; configuration and built-in brands are read-only.
fn database_index(state: &AppState, database: &[DatabaseBrand], id: &str) -> Result<usize, ApiError> {
  if let Some(index) = database.iter().position(|brand| brand.config.id == id) {
    return Ok(index);
  }
  match state.brands.current().by_id(id) {
    Some(brand) if brand.source == BrandSource::Config => Err(ApiError::Conflict(
      "brands of the configuration file are read-only; take the brand over into the database first".into(),
    )),
    Some(_) => Err(ApiError::Conflict("the built-in brand is read-only".into())),
    None => Err(brand_not_found()),
  }
}

/// A new default takes that role from the other database brands.
fn clear_other_defaults(database: &mut [DatabaseBrand], id: &str, is_default: bool) {
  if is_default {
    for brand in database.iter_mut().filter(|brand| brand.config.id != id) {
      brand.config.is_default = false;
    }
  }
}

/// The settings of a request on top of a brand's stored ones; values only the configuration file
/// can set (content security policy, connect origins, mail texts) are kept.
fn apply_settings(
  mut config: BrandConfig,
  id: &str,
  is_default: bool,
  settings: BrandSettings,
) -> BrandConfig {
  config.id = id.to_owned();
  config.is_default = is_default;
  config.display_name = settings.display_name.trim().to_owned();
  config.maybe_organization_name = non_empty(settings.maybe_organization_name);
  config.hostnames = normalized_list(settings.hostnames);
  config.email_domains = normalized_list(settings.email_domains);
  config.colors = BrandColorsConfig {
    light: non_empty_map(settings.colors.light),
    dark: non_empty_map(settings.colors.dark),
  };
  config.login_title = non_empty_map(settings.login_title);
  config.login_text = non_empty_map(settings.login_text);
  config.maybe_support_url = non_empty(settings.links.maybe_support);
  config.maybe_privacy_url = non_empty(settings.links.maybe_privacy);
  config.maybe_terms_url = non_empty(settings.links.maybe_terms);
  config.maybe_imprint_url = non_empty(settings.links.maybe_imprint);
  config.mail.maybe_sender_name = non_empty(settings.mail.maybe_sender_name);
  config.mail.maybe_from_address = non_empty(settings.mail.maybe_from_address);
  clear_file_names(&mut config);
  config
}

fn settings_of(config: &BrandConfig) -> BrandSettings {
  BrandSettings {
    display_name: config.display_name.clone(),
    maybe_organization_name: config.maybe_organization_name.clone(),
    hostnames: config.hostnames.clone(),
    email_domains: config.email_domains.clone(),
    colors: BrandColors {
      light: config.colors.light.clone(),
      dark: config.colors.dark.clone(),
    },
    login_title: config.login_title.clone(),
    login_text: config.login_text.clone(),
    links: BrandLinks {
      maybe_support: config.maybe_support_url.clone(),
      maybe_privacy: config.maybe_privacy_url.clone(),
      maybe_terms: config.maybe_terms_url.clone(),
      maybe_imprint: config.maybe_imprint_url.clone(),
    },
    mail: BrandMailSettings {
      maybe_sender_name: config.mail.maybe_sender_name.clone(),
      maybe_from_address: config.mail.maybe_from_address.clone(),
    },
  }
}

/// Database brands keep their images in the object storage, never as files.
fn clear_file_names(config: &mut BrandConfig) {
  config.maybe_logo = None;
  config.maybe_logo_dark = None;
  config.maybe_icon = None;
  config.maybe_icon_dark = None;
  config.maybe_favicon = None;
  config.maybe_favicon_dark = None;
}

fn non_empty(maybe_text: Option<String>) -> Option<String> {
  maybe_text
    .map(|text| text.trim().to_owned())
    .filter(|text| !text.is_empty())
}

fn non_empty_map(map: BTreeMap<String, String>) -> BTreeMap<String, String> {
  map
    .into_iter()
    .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
    .filter(|(_, value)| !value.is_empty())
    .collect()
}

/// Lower-cased, trimmed, without empty entries and duplicates, in the given order. Entries that
/// are not host names stay as typed, so the brand check names them.
fn normalized_list(entries: Vec<String>) -> Vec<String> {
  let mut seen = BTreeSet::new();
  entries
    .into_iter()
    .map(|entry| {
      let trimmed = entry.trim();
      normalize_host(trimmed)
        .filter(|host| host.len() == trimmed.len())
        .unwrap_or_else(|| trimmed.to_owned())
    })
    .filter(|entry| !entry.is_empty() && seen.insert(entry.clone()))
    .collect()
}

fn object_key(state: &AppState, id: &str, slot: &BrandAssetSlot) -> Result<String, ApiError> {
  let unique = state
    .ids
    .next_snowflake()
    .map_err(|error| ApiError::Internal(error.to_string()))?;
  Ok(format!("brands/{id}/{}/{}", slot.as_str(), unique.get()))
}

fn asset_row(id: &str, slot: &str, asset: &StoredAsset) -> StoredBrandAsset {
  StoredBrandAsset {
    brand_id: id.to_owned(),
    slot: slot.to_owned(),
    object_key: asset.object_key.clone(),
    content_type: asset.content_type.to_owned(),
    size_bytes: i64::try_from(asset.bytes.len()).unwrap_or(i64::MAX),
  }
}

/// Removes images nobody refers to any more; a failure leaves an orphan behind, nothing worse.
async fn delete_objects(state: &AppState, keys: &[String]) {
  let Some(store) = &state.maybe_blob_store else {
    return;
  };
  for key in keys {
    if let Err(error) = store.delete_object(key).await {
      warn!(%error, "deleting a replaced brand image failed");
    }
  }
}

fn blob_store(state: &AppState) -> Result<&BlobStore, ApiError> {
  state
    .maybe_blob_store
    .as_deref()
    .ok_or_else(|| ApiError::Unavailable("brand images need the object storage ([storage])".into()))
}

fn invalid(error: BrandError) -> ApiError {
  ApiError::InvalidRequest(error.to_string())
}

fn brand_not_found() -> ApiError {
  ApiError::brand_not_found()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn settings_are_trimmed_and_config_only_values_survive() {
    let mut stored = BrandConfig {
      maybe_content_security_policy: Some("default-src 'self'".into()),
      connect_origins: vec!["https://api.example.com".into()],
      ..BrandConfig::default()
    };
    stored.mail.maybe_invite_subject = Some("Join us".into());
    let settings = BrandSettings {
      display_name: " Brand A ".into(),
      maybe_organization_name: Some("  ".into()),
      hostnames: vec![" Chat.A.example.com ".into(), "chat.a.example.com".into()],
      email_domains: vec!["A.example.com".into()],
      login_title: BTreeMap::from([("en".into(), "Hi".into()), ("de".into(), " ".into())]),
      links: BrandLinks {
        maybe_support: Some("https://support.example.com".into()),
        ..BrandLinks::default()
      },
      ..BrandSettings::default()
    };
    let config = apply_settings(stored, "brand-a", true, settings);
    assert_eq!(config.display_name, "Brand A");
    assert_eq!(config.maybe_organization_name, None);
    assert_eq!(config.hostnames, vec!["chat.a.example.com".to_owned()]);
    assert_eq!(config.email_domains, vec!["a.example.com".to_owned()]);
    assert_eq!(config.login_title.len(), 1);
    assert!(config.is_default);
    assert_eq!(
      config.maybe_content_security_policy.as_deref(),
      Some("default-src 'self'")
    );
    assert_eq!(config.mail.maybe_invite_subject.as_deref(), Some("Join us"));
    assert_eq!(
      settings_of(&config).links.maybe_support.as_deref(),
      Some("https://support.example.com")
    );
  }

  #[test]
  fn malformed_list_entries_are_kept_for_the_brand_check_to_name() {
    assert_eq!(
      normalized_list(vec!["https://x.example.com".into(), "".into()]),
      vec!["https://x.example.com".to_owned()]
    );
  }

  #[test]
  fn only_known_image_types_are_served_from_storage() {
    assert_eq!(known_content_type("image/png"), Some("image/png"));
    assert_eq!(known_content_type("text/html"), None);
  }
}
