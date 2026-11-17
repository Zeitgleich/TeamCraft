//! Profile pictures from the identity provider (issue #2071, docs/admin.md §3.11).
//!
//! While the workspace policy takes pictures from the provider (`directory.avatar_source`), the
//! server imports them: from the `picture` claim of a member's token and from the `photos` of a SCIM
//! user. Like an upload, a picture is decoded and re-encoded as a square PNG here
//! ([`super::profiles::sanitized_avatar`]), so what is stored and served has no metadata and a size
//! the server chose. Which picture shows is decided when it is read ([`super::profiles`]).
//!
//! The server fetches a picture URL itself, so the URL must not reach into its own network (SSRF):
//! https only, public addresses only (checked by text and again when connecting), except the
//! identity provider's own origin, which the operator configured. Phase timeouts and the avatar
//! size limit bound a fetch. Imports run in the background, one per person at a time; a sign-in that
//! names the picture already imported costs a map lookup. Picture URLs and bytes are never logged.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use teamcraft_proto::{AvatarSource, UserId};
use tracing::{debug, warn};
use url::Url;

use super::profiles::{blob_store, publish_update, put_png, sanitized_avatar};
use super::space_images::raster_type;
use super::storage_cleanup::delete_or_queue;
use crate::error::ApiError;
use crate::outbound::{PublicOnlyResolver, check_public_url};
use crate::state::AppState;

/// People whose picture source this run remembers; past it the memory starts over (a sign-in then
/// costs one database read again).
const MAX_REMEMBERED: usize = 10_000;
/// A picture that failed to import is tried again after this long (or when its source changes).
const RETRY_AFTER: Duration = Duration::from_secs(60 * 60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REDIRECTS: usize = 3;

/// What the server knows about importing pictures this run.
#[derive(Default)]
pub struct ProviderPictures {
  maybe_clients: OnceLock<Option<Clients>>,
  remembered: Mutex<HashMap<UserId, Remembered>>,
  running: Mutex<HashSet<UserId>>,
}

/// Where a picture comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PictureSource {
  /// A URL the server fetches.
  Url(String),
  /// A `data:` URI a SCIM client sent; its source is the hash.
  Inline { data_uri: String, hash: String },
}

/// The last source seen of a person, and whether importing it failed (and when).
#[derive(Debug, Clone)]
struct Remembered {
  source: String,
  maybe_failed_at: Option<Instant>,
}

struct Clients {
  /// Public addresses only, redirects only to public URLs.
  public: reqwest::Client,
  /// For the identity provider's own origin, which may be on a private network; no redirects.
  provider: reqwest::Client,
}

/// Why a picture was not imported.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderPictureError {
  #[error("the picture URL is not one the server fetches from")]
  RefusedUrl,
  #[error("the picture could not be fetched")]
  Unreachable,
  #[error("the picture server answered {0}")]
  Status(u16),
  #[error("the picture is larger than the avatar limit")]
  TooLarge,
  #[error("the inline picture is not a base64 data URI")]
  BadDataUri,
  #[error("the picture is not a PNG, JPEG or WebP image")]
  NotAPicture,
  #[error("storing the picture failed")]
  Storage,
}

impl ProviderPictures {
  /// A member signed in with a token; `maybe_url` is its `picture` claim. A token without one
  /// leaves the picture alone: providers often leave the claim out of access tokens.
  pub fn note_sign_in(state: &AppState, user: UserId, maybe_url: Option<&str>) {
    let Some(url) = maybe_url else {
      return;
    };
    if !is_from_provider(state) {
      return;
    }
    let source = PictureSource::Url(url.to_owned());
    if state.provider_pictures.is_settled(user, &source) {
      return;
    }
    spawn_import(state, user, source);
  }

  /// A SCIM client wrote a user with `maybe_photo` (`photos`, absent: none). A changed photo is
  /// imported in the background; a removed one is removed now.
  pub async fn note_scim_photo(
    state: &AppState,
    user: UserId,
    maybe_photo: Option<&str>,
  ) -> Result<(), ApiError> {
    if !is_from_provider(state) {
      return Ok(());
    }
    let maybe_source = maybe_photo.map(PictureSource::from_scim);
    let stored = state.database.provider_picture_source(user).await?;
    match maybe_source {
      None if stored.is_some() => remove(state, user).await,
      None => Ok(()),
      // A PATCH that left the photo alone sends back the stored source.
      Some(source) if Some(source.key()) == stored.as_deref() => Ok(()),
      Some(source) => {
        spawn_import(state, user, source);
        Ok(())
      },
    }
  }

  /// Whether `source` is the person's imported picture, or failed to import a short while ago.
  fn is_settled(&self, user: UserId, source: &PictureSource) -> bool {
    lock(&self.remembered).get(&user).is_some_and(|remembered| {
      remembered.source == source.key()
        && remembered
          .maybe_failed_at
          .is_none_or(|failed_at| failed_at.elapsed() < RETRY_AFTER)
    })
  }

  fn remember(&self, user: UserId, source: &PictureSource, has_failed: bool) {
    let mut remembered = lock(&self.remembered);
    if remembered.len() >= MAX_REMEMBERED && !remembered.contains_key(&user) {
      remembered.clear();
    }
    remembered.insert(
      user,
      Remembered {
        source: source.key().to_owned(),
        maybe_failed_at: has_failed.then(Instant::now),
      },
    );
  }

  fn clients(&self) -> Option<&Clients> {
    self
      .maybe_clients
      .get_or_init(|| match Clients::build() {
        Ok(clients) => Some(clients),
        Err(error) => {
          warn!(%error, "the client for provider pictures could not be built");
          None
        },
      })
      .as_ref()
  }
}

impl PictureSource {
  fn from_scim(value: &str) -> Self {
    if value
      .get(..5)
      .is_some_and(|scheme| scheme.eq_ignore_ascii_case("data:"))
    {
      Self::Inline {
        data_uri: value.to_owned(),
        hash: teamcraft_crypto::account::subresource_integrity(value.as_bytes()),
      }
    } else {
      Self::Url(value.to_owned())
    }
  }

  /// What the database keeps to recognize the picture: the URL, or the hash of an inline one.
  fn key(&self) -> &str {
    match self {
      Self::Url(url) => url,
      Self::Inline { hash, .. } => hash,
    }
  }
}

impl Clients {
  fn build() -> Result<Self, reqwest::Error> {
    let builder = || {
      reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .timeout(TOTAL_TIMEOUT)
        // A proxy resolves names itself, past the public-only resolver.
        .no_proxy()
        .user_agent(concat!(
          "TeamCraft/",
          env!("CARGO_PKG_VERSION"),
          " (profile pictures)"
        ))
    };
    let follow_public = reqwest::redirect::Policy::custom(|attempt| {
      if attempt.previous().len() <= MAX_REDIRECTS && check_public_url(attempt.url()).is_ok() {
        attempt.follow()
      } else {
        attempt.stop()
      }
    });
    Ok(Self {
      public: builder()
        .redirect(follow_public)
        .dns_resolver(Arc::new(PublicOnlyResolver))
        .build()?,
      provider: builder().redirect(reqwest::redirect::Policy::none()).build()?,
    })
  }
}

fn is_from_provider(state: &AppState) -> bool {
  state.policy.get().directory.avatar_source == AvatarSource::IdentityProvider
}

/// Imports `source` as `user`'s provider picture in the background, unless an import of theirs runs.
fn spawn_import(state: &AppState, user: UserId, source: PictureSource) {
  if !lock(&state.provider_pictures.running).insert(user) {
    return;
  }
  let state = state.clone();
  tokio::spawn(async move {
    let result = import(&state, user, &source).await;
    lock(&state.provider_pictures.running).remove(&user);
    match result {
      Ok(()) => state.provider_pictures.remember(user, &source, false),
      Err(error) => {
        // Not the URL: it may carry a token of the provider's.
        warn!(user_id = %user, %error, "importing a picture from the identity provider failed");
        state.provider_pictures.remember(user, &source, true);
      },
    }
  });
}

async fn import(state: &AppState, user: UserId, source: &PictureSource) -> Result<(), ImportError> {
  if state.database.provider_picture_source(user).await?.as_deref() == Some(source.key()) {
    return Ok(());
  }
  let max_bytes = state.settings.get().limits.max_avatar_bytes;
  let bytes = match source {
    PictureSource::Url(url) => fetch(state, url, max_bytes).await?,
    PictureSource::Inline { data_uri, .. } => decode_data_uri(data_uri, max_bytes)?,
  };
  let declared = raster_type(&bytes).ok_or(ProviderPictureError::NotAPicture)?;
  let png = sanitized_avatar(state, declared.to_owned(), Bytes::from(bytes)).await?;
  let store = blob_store(state)?;
  let avatar = put_png(state, store, user, png).await?;
  match state
    .database
    .set_provider_avatar(user, Some((&avatar, source.key())))
    .await
  {
    Ok(Some(replaced)) => delete_or_queue(state, store, &replaced).await,
    Ok(None) => {},
    Err(error) => {
      delete_or_queue(state, store, &avatar.key).await;
      return Err(error.into());
    },
  }
  debug!(user_id = %user, "imported a picture from the identity provider");
  publish_update(state, user).await?;
  Ok(())
}

/// Removes `user`'s provider picture: their own picture or the initials show again.
async fn remove(state: &AppState, user: UserId) -> Result<(), ApiError> {
  let store = blob_store(state)?;
  if let Some(key) = state.database.set_provider_avatar(user, None).await? {
    delete_or_queue(state, store, &key).await;
  }
  lock(&state.provider_pictures.remembered).remove(&user);
  publish_update(state, user).await
}

/// Fetches a picture of at most `max_bytes`: from the provider's own origin, or a public URL.
async fn fetch(state: &AppState, text: &str, max_bytes: u64) -> Result<Vec<u8>, ProviderPictureError> {
  let url: Url = text.parse().map_err(|_| ProviderPictureError::RefusedUrl)?;
  let clients = state
    .provider_pictures
    .clients()
    .ok_or(ProviderPictureError::Unreachable)?;
  let client = if is_provider_origin(&url, &state.settings.get().sign_in.issuer) {
    &clients.provider
  } else {
    check_public_url(&url).map_err(|_| ProviderPictureError::RefusedUrl)?;
    &clients.public
  };
  let mut response = client
    .get(url)
    .send()
    .await
    .map_err(|_| ProviderPictureError::Unreachable)?;
  if !response.status().is_success() {
    return Err(ProviderPictureError::Status(response.status().as_u16()));
  }
  if response.content_length().is_some_and(|length| length > max_bytes) {
    return Err(ProviderPictureError::TooLarge);
  }
  let mut body = Vec::new();
  while let Some(chunk) = response
    .chunk()
    .await
    .map_err(|_| ProviderPictureError::Unreachable)?
  {
    if (body.len() + chunk.len()) as u64 > max_bytes {
      return Err(ProviderPictureError::TooLarge);
    }
    body.extend_from_slice(&chunk);
  }
  Ok(body)
}

/// Whether `url` is on the identity provider's own origin (scheme, host and port of the issuer).
/// The operator configured that address, so it may be private.
fn is_provider_origin(url: &Url, issuer: &str) -> bool {
  let Ok(issuer) = issuer.parse::<Url>() else {
    return false;
  };
  url.username().is_empty()
    && url.password().is_none()
    && url.origin() == issuer.origin()
    && matches!(url.scheme(), "https" | "http")
}

/// The bytes of a `data:<type>;base64,<data>` URI of at most `max_bytes`.
fn decode_data_uri(data_uri: &str, max_bytes: u64) -> Result<Vec<u8>, ProviderPictureError> {
  let (header, data) = data_uri.split_once(',').ok_or(ProviderPictureError::BadDataUri)?;
  if !header.to_ascii_lowercase().ends_with(";base64") {
    return Err(ProviderPictureError::BadDataUri);
  }
  // Base64 is a third larger than what it encodes.
  if data.len() as u64 / 4 * 3 > max_bytes.saturating_add(3) {
    return Err(ProviderPictureError::TooLarge);
  }
  let bytes = STANDARD
    .decode(data.trim())
    .map_err(|_| ProviderPictureError::BadDataUri)?;
  if bytes.len() as u64 > max_bytes {
    return Err(ProviderPictureError::TooLarge);
  }
  Ok(bytes)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  // A panic while holding the lock leaves a map that is still consistent (single inserts/removes).
  mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// An import failed for a reason of the picture or of the server.
#[derive(Debug, thiserror::Error)]
enum ImportError {
  #[error(transparent)]
  Picture(#[from] ProviderPictureError),
  #[error(transparent)]
  Api(#[from] ApiError),
}

impl From<teamcraft_db::DbError> for ImportError {
  fn from(error: teamcraft_db::DbError) -> Self {
    Self::Api(error.into())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  use teamcraft_proto::Id;

  #[test]
  fn only_the_issuers_own_origin_counts_as_the_provider() {
    let issuer = "https://sso.example.com/realms/team";
    for good in [
      "https://sso.example.com/realms/team/pictures/1.png",
      "https://sso.example.com:443/other",
    ] {
      assert!(is_provider_origin(&good.parse().unwrap(), issuer), "{good}");
    }
    for bad in [
      "https://sso.example.com:8443/x",
      "http://sso.example.com/x",
      "https://evil.example.com/x",
      "https://user:pw@sso.example.com/x",
      "https://sso.example.com.evil.example/x",
    ] {
      assert!(!is_provider_origin(&bad.parse().unwrap(), issuer), "{bad}");
    }
    assert!(!is_provider_origin(
      &"https://a.example/x".parse().unwrap(),
      "not a url"
    ));
  }

  mod data_uris {
    use super::*;

    #[test]
    fn base64_data_uris_decode_within_the_limit() {
      assert_eq!(
        decode_data_uri("data:image/png;base64,AAEC", 10),
        Ok(vec![0, 1, 2])
      );
      assert_eq!(
        decode_data_uri("DATA:image/png;BASE64,AAEC", 10),
        Ok(vec![0, 1, 2])
      );
    }

    #[test]
    fn other_data_uris_are_refused() {
      assert_eq!(
        decode_data_uri("data:image/png,raw", 10),
        Err(ProviderPictureError::BadDataUri)
      );
      assert_eq!(
        decode_data_uri("data:image/png;base64", 10),
        Err(ProviderPictureError::BadDataUri)
      );
      assert_eq!(
        decode_data_uri("data:image/png;base64,!!!!", 10),
        Err(ProviderPictureError::BadDataUri)
      );
      assert_eq!(
        decode_data_uri(&format!("data:image/png;base64,{}", "A".repeat(4000)), 100),
        Err(ProviderPictureError::TooLarge)
      );
    }
  }

  mod sources {
    use super::*;

    #[test]
    fn inline_photos_are_known_by_their_hash_and_urls_by_themselves() {
      let url = PictureSource::from_scim("https://pictures.example.com/1.jpg");
      assert_eq!(url.key(), "https://pictures.example.com/1.jpg");
      let inline = PictureSource::from_scim("data:image/png;base64,AAEC");
      assert!(inline.key().starts_with("sha384-"), "{}", inline.key());
      assert_ne!(
        inline.key(),
        PictureSource::from_scim("data:image/png;base64,AAED").key()
      );
    }

    #[test]
    fn a_settled_source_is_retried_only_after_a_failure_has_aged() {
      let pictures = ProviderPictures::default();
      let user = UserId::from_snowflake(teamcraft_proto::Snowflake::new(7).unwrap());
      let source = PictureSource::Url("https://pictures.example.com/1.jpg".into());
      assert!(!pictures.is_settled(user, &source));
      pictures.remember(user, &source, false);
      assert!(pictures.is_settled(user, &source));
      let other = PictureSource::Url("https://pictures.example.com/2.jpg".into());
      assert!(!pictures.is_settled(user, &other));
      pictures.remember(user, &other, true);
      assert!(pictures.is_settled(user, &other));
      if let Some(entry) = lock(&pictures.remembered).get_mut(&user) {
        entry.maybe_failed_at = Instant::now().checked_sub(RETRY_AFTER + Duration::from_secs(1));
      }
      assert!(!pictures.is_settled(user, &other));
    }
  }
}
