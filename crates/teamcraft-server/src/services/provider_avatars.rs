//! Profile pictures from the identity provider (issue #2071, docs/admin.md §3.12).
//!
//! While the workspace policy takes pictures from the provider (`directory.avatar_source`), the
//! server imports them: from the `photos` of a SCIM user, and from the `picture` claim of a member's
//! token while no SCIM client manages them (SCIM wins, so the two never take turns). Like an upload,
//! a picture is decoded and re-encoded as a square PNG here ([`super::profiles::sanitized_avatar`]),
//! so what is stored and served has no metadata and a size the server chose. Which picture shows is
//! decided when it is read ([`super::profiles`]).
//!
//! The server fetches a picture URL itself, so the URL must not reach into its own network (SSRF):
//! https only, public addresses only (checked by text and again when connecting), except the
//! identity provider's own origin, which the operator configured. Phase timeouts and the avatar
//! size limit bound a fetch. Picture URLs and bytes are never logged.
//!
//! Imports run in the background (issue #2125): one worker per person at a time, which takes up the
//! newest source sent meanwhile; at most `limits.max_concurrent_avatar_imports` at once, waiting for
//! the decoding pool instead of failing as busy. Every new source and every removal raises the
//! person's generation, and an import stores its picture only while its generation is still the
//! current one, so an older import never overwrites a newer picture or a removal. A sign-in that
//! names the picture already imported costs a map lookup.
//!
//! `provider_picture_source` (migration 0109) holds the URL, or for a `data:` URI its SRI hash
//! (`sha384-…`, [`teamcraft_crypto::account::subresource_integrity`]); the migration's comment says
//! `sha256:`, which no code ever wrote.

use std::collections::HashMap;
#[cfg(feature = "integration")]
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use teamcraft_proto::{AvatarSource, UserId};
use tokio::sync::{Semaphore, oneshot};
use tracing::{debug, warn};
use url::Url;

use super::profiles::{blob_store, publish_update, put_png, sanitized_avatar};
use super::space_images::{WhenBusy, raster_type};
use super::storage_cleanup::delete_or_queue;
use crate::error::ApiError;
use crate::outbound::{PublicOnlyResolver, check_public_url};
use crate::state::AppState;

/// People whose picture source this run remembers; past it the one noted longest ago is forgotten
/// (their next sign-in costs one database read again).
const MAX_REMEMBERED: usize = 10_000;
/// A picture that failed to import is tried again after this long (or when its source changes).
const RETRY_AFTER: Duration = Duration::from_secs(60 * 60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REDIRECTS: usize = 3;
/// How the key of an inline photo starts ([`teamcraft_crypto::account::subresource_integrity`]);
/// no picture URL does.
const INLINE_HASH_PREFIX: &str = "sha384-";

/// What the server knows about importing pictures this run.
pub struct ProviderPictures {
  maybe_clients: OnceLock<Option<Clients>>,
  remembered: Mutex<HashMap<UserId, Remembered>>,
  /// People with a running import worker, and what it does next.
  workers: Mutex<HashMap<UserId, Worker>>,
  /// Imports at once (`limits.max_concurrent_avatar_imports`).
  permits: Arc<Semaphore>,
  /// Held around the check and write that end an import and around a removal, so a removal can't
  /// land between an import's check of its generation and its write.
  commits: tokio::sync::Mutex<()>,
  #[cfg(feature = "integration")]
  probe: ImportProbe,
}

/// Where a picture comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PictureSource {
  /// A URL the server fetches.
  Url(String),
  /// A `data:` URI a SCIM client sent; its source is the hash.
  Inline { data_uri: String, hash: String },
  /// The hash of an inline photo the server holds, as a SCIM PATCH that left `photos` alone sends
  /// it back ([`ProviderPictures::scim_photo`]).
  Known(String),
}

/// Who sent a picture source. SCIM takes precedence: while a SCIM client manages a person, the
/// `picture` claim of their token is ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
  Claim,
  Scim,
}

/// A source to import, and who sent it.
#[derive(Debug, Clone)]
struct Job {
  origin: Origin,
  source: PictureSource,
}

/// The import worker of one person.
#[derive(Debug)]
struct Worker {
  /// Raised by every newer source and every removal; an import stores only while it is unchanged.
  generation: u64,
  /// What the worker does now or next, newest first: the one a newer source has to differ from.
  newest: (Origin, String),
  /// The newest SCIM photo this worker holds; `None` once a removal cancelled it.
  maybe_scim_key: Option<String>,
  maybe_next: Option<Job>,
}

/// What this run remembers of a person's picture.
#[derive(Debug, Clone)]
struct Remembered {
  /// SCIM once a SCIM client wrote the person: their claim no longer counts.
  origin: Origin,
  /// The source last imported or tried; `None` before any or after a removal.
  maybe_source: Option<String>,
  maybe_failed_at: Option<Instant>,
  noted_at: Instant,
}

/// How an import ended without an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Imported {
  Stored,
  /// The source is already the stored picture.
  Unchanged,
  /// A newer source or a removal came meanwhile; the picture was thrown away.
  Superseded,
  /// A claim of a person a SCIM client manages.
  LeftToScim,
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
  /// At most `max_concurrent_imports` imports at once (at least one).
  pub fn new(max_concurrent_imports: usize) -> Self {
    Self {
      maybe_clients: OnceLock::new(),
      remembered: Mutex::default(),
      workers: Mutex::default(),
      permits: Arc::new(Semaphore::new(max_concurrent_imports.max(1))),
      commits: tokio::sync::Mutex::new(()),
      #[cfg(feature = "integration")]
      probe: ImportProbe::new(),
    }
  }

  /// A member signed in with a token; `maybe_url` is its `picture` claim. A token without one
  /// leaves the picture alone: providers often leave the claim out of access tokens.
  pub fn note_sign_in(state: &AppState, user: UserId, maybe_url: Option<&str>) {
    let Some(url) = maybe_url else {
      return;
    };
    if !is_from_provider(state) {
      return;
    }
    let job = Job {
      origin: Origin::Claim,
      source: PictureSource::Url(url.to_owned()),
    };
    if state.provider_pictures.is_settled(user, &job) {
      return;
    }
    if let Some(generation) = state.provider_pictures.start_or_hand_over(user, &job) {
      spawn_worker(state.clone(), user, job, generation, None);
    }
  }

  /// A SCIM client wrote a user with `maybe_photo` (`photos`, absent: none). A changed photo is
  /// imported in the background; a removed one is removed now. Waits for an import permit, so a
  /// bulk sync is slowed down to what the server imports instead of piling up pictures in memory.
  pub async fn note_scim_photo(
    state: &AppState,
    user: UserId,
    maybe_photo: Option<&str>,
  ) -> Result<(), ApiError> {
    if !is_from_provider(state) {
      return Ok(());
    }
    let pictures = &state.provider_pictures;
    pictures.note_scim_managed(user);
    let Some(photo) = maybe_photo else {
      return remove(state, user).await;
    };
    let job = Job {
      origin: Origin::Scim,
      source: PictureSource::from_scim(photo),
    };
    if pictures.is_settled(user, &job) {
      return Ok(());
    }
    // Registered at once, so a retry hands over and a removal cancels it while it waits.
    if let Some(generation) = pictures.start_or_hand_over(user, &job) {
      let (admitted, is_admitted) = oneshot::channel();
      spawn_worker(state.clone(), user, job, generation, Some(admitted));
      // An error: the worker ended without a permit; nothing to wait for.
      let _ = is_admitted.await;
    }
    Ok(())
  }

  /// The photo a SCIM PATCH starts from: the newest one an import holds, else the stored source.
  /// For an inline photo that is its `sha384-` hash, which [`PictureSource::from_scim`] reads back
  /// as [`PictureSource::Known`]: recognized, never fetched.
  pub async fn scim_photo(state: &AppState, user: UserId) -> Result<Option<String>, ApiError> {
    let maybe_pending = lock(&state.provider_pictures.workers)
      .get(&user)
      .and_then(|worker| worker.maybe_scim_key.clone());
    if let Some(key) = maybe_pending {
      return Ok(Some(key));
    }
    Ok(state.database.provider_picture_source(user).await?)
  }

  /// A SCIM client removed `user`: their claim counts again (their account is deactivated or
  /// deleted, so mostly for a later re-link through sign-in).
  pub fn note_scim_removed(&self, user: UserId) {
    lock(&self.remembered).remove(&user);
  }

  /// What integration tests watch and steer of the imports.
  #[cfg(feature = "integration")]
  pub fn probe(&self) -> &ImportProbe {
    &self.probe
  }

  /// Whether nothing is to do for `job`: a claim of a person SCIM manages, a source being imported,
  /// or one that failed a short while ago. A claim already imported counts too; a SCIM photo is
  /// compared with the stored source instead, which the database decides.
  fn is_settled(&self, user: UserId, job: &Job) -> bool {
    let is_running = lock(&self.workers)
      .get(&user)
      .is_some_and(|worker| worker.newest == (job.origin, job.source.key().to_owned()));
    is_running
      || lock(&self.remembered).get(&user).is_some_and(|remembered| {
        let is_same = remembered.maybe_source.as_deref() == Some(job.source.key());
        match (job.origin, remembered.maybe_failed_at) {
          (Origin::Claim, _) if remembered.origin == Origin::Scim => true,
          (_, Some(failed_at)) => is_same && failed_at.elapsed() < RETRY_AFTER,
          (Origin::Claim, None) => is_same,
          (Origin::Scim, None) => false,
        }
      })
  }

  /// Hands `job` to `user`'s running worker, which takes it up next. Whether one ran.
  fn hand_over(&self, user: UserId, job: &Job) -> bool {
    let mut workers = lock(&self.workers);
    let Some(worker) = workers.get_mut(&user) else {
      return false;
    };
    let is_scim_pending = worker.newest.0 == Origin::Scim;
    // A claim never displaces a SCIM photo; the worker drops claims of SCIM-managed people itself.
    if job.origin == Origin::Scim || !is_scim_pending {
      worker.generation += 1;
      worker.newest = (job.origin, job.source.key().to_owned());
      if job.origin == Origin::Scim {
        worker.maybe_scim_key = Some(job.source.key().to_owned());
      }
      worker.maybe_next = Some(job.clone());
    }
    true
  }

  /// Starts a worker for `user` and returns its generation, or hands `job` to the one that runs.
  fn start_or_hand_over(&self, user: UserId, job: &Job) -> Option<u64> {
    let mut workers = lock(&self.workers);
    if workers.contains_key(&user) {
      drop(workers);
      self.hand_over(user, job);
      return None;
    }
    workers.insert(
      user,
      Worker {
        generation: 0,
        newest: (job.origin, job.source.key().to_owned()),
        maybe_scim_key: (job.origin == Origin::Scim).then(|| job.source.key().to_owned()),
        maybe_next: None,
      },
    );
    Some(0)
  }

  /// The worker's next job and its generation, or `None` after removing the finished worker.
  fn take_next(&self, user: UserId) -> Option<(Job, u64)> {
    let mut workers = lock(&self.workers);
    let worker = workers.get_mut(&user)?;
    match worker.maybe_next.take() {
      Some(job) => Some((job, worker.generation)),
      None => {
        workers.remove(&user);
        None
      },
    }
  }

  /// Whether an import of `generation` may still store its picture.
  fn is_current(&self, user: UserId, generation: u64) -> bool {
    lock(&self.workers)
      .get(&user)
      .is_some_and(|worker| worker.generation == generation)
  }

  /// A removal: the running import and the next one must not store anything.
  fn cancel(&self, user: UserId) {
    if let Some(worker) = lock(&self.workers).get_mut(&user) {
      worker.generation += 1;
      worker.maybe_next = None;
      worker.maybe_scim_key = None;
      worker.newest = (Origin::Scim, String::new());
    }
  }

  /// From now on SCIM decides `user`'s picture (until the server restarts; then the worker asks
  /// the database).
  fn note_scim_managed(&self, user: UserId) {
    let mut remembered = self.remembered_room(user);
    let entry = remembered.entry(user).or_insert_with(|| Remembered {
      origin: Origin::Scim,
      maybe_source: None,
      maybe_failed_at: None,
      noted_at: Instant::now(),
    });
    if entry.origin == Origin::Claim {
      *entry = Remembered {
        origin: Origin::Scim,
        maybe_source: None,
        maybe_failed_at: None,
        noted_at: Instant::now(),
      };
    }
  }

  fn remember(&self, user: UserId, job: &Job, outcome: &Result<Imported, ImportError>) {
    let entry = match outcome {
      Ok(Imported::Superseded) => return,
      Ok(Imported::LeftToScim) => return self.note_scim_managed(user),
      _ => Remembered {
        origin: job.origin,
        maybe_source: Some(job.source.key().to_owned()),
        maybe_failed_at: outcome.is_err().then(Instant::now),
        noted_at: Instant::now(),
      },
    };
    let mut remembered = self.remembered_room(user);
    // A claim's result never takes SCIM's place.
    if entry.origin == Origin::Claim
      && remembered
        .get(&user)
        .is_some_and(|known| known.origin == Origin::Scim)
    {
      return;
    }
    remembered.insert(user, entry);
  }

  fn forget_source(&self, user: UserId) {
    if let Some(entry) = lock(&self.remembered).get_mut(&user) {
      entry.maybe_source = None;
      entry.maybe_failed_at = None;
    }
  }

  /// The memory, with room for `user`: past the limit, the entry noted longest ago goes.
  fn remembered_room(&self, user: UserId) -> MutexGuard<'_, HashMap<UserId, Remembered>> {
    let mut remembered = lock(&self.remembered);
    if remembered.len() >= MAX_REMEMBERED
      && !remembered.contains_key(&user)
      && let Some(oldest) = remembered
        .iter()
        .min_by_key(|(_, entry)| entry.noted_at)
        .map(|(user, _)| *user)
    {
      remembered.remove(&oldest);
    }
    remembered
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
    if value.starts_with(INLINE_HASH_PREFIX) {
      return Self::Known(value.to_owned());
    }
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
      Self::Url(url) | Self::Known(url) => url,
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

/// Runs `user`'s imports in the background: `job`, then whatever newer source came meanwhile. The
/// worker is registered before it waits for an import permit, so a source or a removal that comes
/// while it waits is handed over or cancels it. `maybe_admitted` is told once the worker holds a
/// permit or needed none, which a SCIM write waits for.
fn spawn_worker(
  state: AppState,
  user: UserId,
  job: Job,
  generation: u64,
  maybe_admitted: Option<oneshot::Sender<()>>,
) {
  #[cfg(feature = "integration")]
  state.provider_pictures.probe.note_spawned();
  tokio::spawn(async move {
    let pictures = Arc::clone(&state.provider_pictures);
    let mut guard = WorkerGuard {
      pictures: Arc::clone(&pictures),
      user,
      is_finished: false,
    };
    let mut maybe_admitted = maybe_admitted;
    let mut maybe_permit = None;
    let (mut job, mut generation) = (job, generation);
    loop {
      let outcome = match settled_without_import(&state, user, &job).await {
        Ok(Some(imported)) => Ok(imported),
        Err(error) => Err(error),
        Ok(None) => {
          if maybe_permit.is_none() {
            // Invariant: the server never closes the semaphore.
            let Ok(permit) = Arc::clone(&pictures.permits).acquire_owned().await else {
              return;
            };
            maybe_permit = Some(permit);
          }
          admit(&mut maybe_admitted);
          #[cfg(feature = "integration")]
          pictures.probe.note_started();
          let outcome = import(&state, user, &job, generation).await;
          #[cfg(feature = "integration")]
          pictures.probe.note_stopped();
          outcome
        },
      };
      admit(&mut maybe_admitted);
      if let Err(error) = &outcome {
        // Not the URL: it may carry a token of the provider's.
        warn!(user_id = %user, %error, "importing a picture from the identity provider failed");
      }
      pictures.remember(user, &job, &outcome);
      #[cfg(feature = "integration")]
      pictures.probe.note_finished(&outcome);
      match pictures.take_next(user) {
        Some(next) => (job, generation) = next,
        None => break,
      }
    }
    guard.is_finished = true;
  });
}

fn admit(maybe_admitted: &mut Option<oneshot::Sender<()>>) {
  if let Some(admitted) = maybe_admitted.take() {
    // The SCIM write may have ended meanwhile (its client went away); the import goes on.
    let _ = admitted.send(());
  }
}

/// How `job` ends without fetching anything: a claim of a person SCIM manages, or the stored
/// picture's own source. `None`: it needs an import.
async fn settled_without_import(
  state: &AppState,
  user: UserId,
  job: &Job,
) -> Result<Option<Imported>, ImportError> {
  if job.origin == Origin::Claim && state.database.is_scim_managed(user).await? {
    return Ok(Some(Imported::LeftToScim));
  }
  if state.database.provider_picture_source(user).await?.as_deref() == Some(job.source.key()) {
    return Ok(Some(Imported::Unchanged));
  }
  Ok(None)
}

/// Removes the worker's entry if its task ends early (a panic), so the person's imports don't stop
/// for good.
struct WorkerGuard {
  pictures: Arc<ProviderPictures>,
  user: UserId,
  is_finished: bool,
}

impl Drop for WorkerGuard {
  fn drop(&mut self) {
    if !self.is_finished {
      lock(&self.pictures.workers).remove(&self.user);
    }
  }
}

async fn import(state: &AppState, user: UserId, job: &Job, generation: u64) -> Result<Imported, ImportError> {
  let max_bytes = state.settings.get().limits.max_avatar_bytes;
  let bytes = match &job.source {
    PictureSource::Url(url) => fetch(state, url, max_bytes).await?,
    PictureSource::Inline { data_uri, .. } => decode_data_uri(data_uri, max_bytes)?,
    // Only the stored or queued picture's own hash comes back this way; anything else is unknown.
    PictureSource::Known(_) => return Err(ProviderPictureError::RefusedUrl.into()),
  };
  let declared = raster_type(&bytes).ok_or(ProviderPictureError::NotAPicture)?;
  let png = sanitized_avatar(state, declared.to_owned(), Bytes::from(bytes), WhenBusy::Wait).await?;
  let store = blob_store(state)?;
  let avatar = put_png(state, store, user, png).await?;
  #[cfg(feature = "integration")]
  state.provider_pictures.probe.before_commit().await;
  let pictures = &state.provider_pictures;
  let commit = pictures.commits.lock().await;
  if !pictures.is_current(user, generation) {
    drop(commit);
    delete_or_queue(state, store, &avatar.key).await;
    return Ok(Imported::Superseded);
  }
  let written = state
    .database
    .set_provider_avatar(user, Some((&avatar, job.source.key())))
    .await;
  drop(commit);
  match written {
    Ok(Some(replaced)) => delete_or_queue(state, store, &replaced).await,
    Ok(None) => {},
    Err(error) => {
      delete_or_queue(state, store, &avatar.key).await;
      return Err(error.into());
    },
  }
  debug!(user_id = %user, "imported a picture from the identity provider");
  publish_update(state, user).await?;
  Ok(Imported::Stored)
}

/// Removes `user`'s provider picture: their own picture or the initials show again. A running
/// import of theirs stores nothing afterwards.
async fn remove(state: &AppState, user: UserId) -> Result<(), ApiError> {
  let pictures = &state.provider_pictures;
  let maybe_replaced = {
    let _commit = pictures.commits.lock().await;
    pictures.cancel(user);
    match state.database.provider_picture_source(user).await? {
      Some(_) => state.database.set_provider_avatar(user, None).await?,
      None => None,
    }
  };
  pictures.forget_source(user);
  if let Some(key) = maybe_replaced {
    delete_or_queue(state, blob_store(state)?, &key).await;
    publish_update(state, user).await?;
  }
  Ok(())
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

/// What integration tests watch and steer of the imports (issue #2125), without sleeping or polling.
#[cfg(feature = "integration")]
pub struct ImportProbe {
  /// While set, every import waits before it stores its picture: it adds a permit to `paused`, then
  /// takes one from `resume`.
  pub is_pausing: AtomicBool,
  pub paused: Semaphore,
  pub resume: Semaphore,
  /// A permit per finished job, whatever its outcome (also one settled without an import).
  pub finished: Semaphore,
  /// A permit per worker started (registered for a person).
  pub spawned: Semaphore,
  /// Imports that fetched or decoded a picture.
  pub started: AtomicUsize,
  pub stored: AtomicUsize,
  pub superseded: AtomicUsize,
  pub left_to_scim: AtomicUsize,
  pub failed: AtomicUsize,
  /// The most imports that ran at once.
  pub most_at_once: AtomicUsize,
  running: AtomicUsize,
}

#[cfg(feature = "integration")]
impl ImportProbe {
  fn new() -> Self {
    Self {
      is_pausing: AtomicBool::new(false),
      paused: Semaphore::new(0),
      resume: Semaphore::new(0),
      finished: Semaphore::new(0),
      spawned: Semaphore::new(0),
      started: AtomicUsize::new(0),
      stored: AtomicUsize::new(0),
      superseded: AtomicUsize::new(0),
      left_to_scim: AtomicUsize::new(0),
      failed: AtomicUsize::new(0),
      most_at_once: AtomicUsize::new(0),
      running: AtomicUsize::new(0),
    }
  }

  fn note_started(&self) {
    self.started.fetch_add(1, Ordering::SeqCst);
    let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
    self.most_at_once.fetch_max(now, Ordering::SeqCst);
  }

  fn note_spawned(&self) {
    self.spawned.add_permits(1);
  }

  fn note_stopped(&self) {
    self.running.fetch_sub(1, Ordering::SeqCst);
  }

  fn note_finished(&self, outcome: &Result<Imported, ImportError>) {
    let counter = match outcome {
      Ok(Imported::Stored) => Some(&self.stored),
      Ok(Imported::Superseded) => Some(&self.superseded),
      Ok(Imported::LeftToScim) => Some(&self.left_to_scim),
      Ok(Imported::Unchanged) => None,
      Err(_) => Some(&self.failed),
    };
    if let Some(counter) = counter {
      counter.fetch_add(1, Ordering::SeqCst);
    }
    self.finished.add_permits(1);
  }

  async fn before_commit(&self) {
    if self.is_pausing.load(Ordering::SeqCst) {
      self.paused.add_permits(1);
      if let Ok(permit) = self.resume.acquire().await {
        permit.forget();
      }
    }
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
      // A PATCH that left photos alone sends the hash back: recognized, never fetched.
      assert_eq!(
        PictureSource::from_scim(inline.key()),
        PictureSource::Known(inline.key().to_owned())
      );
    }

    #[test]
    fn a_settled_source_is_retried_only_after_a_failure_has_aged() {
      let pictures = ProviderPictures::new(1);
      let user = user(7);
      let first = claim("https://pictures.example.com/1.jpg");
      assert!(!pictures.is_settled(user, &first));
      pictures.remember(user, &first, &Ok(Imported::Stored));
      assert!(pictures.is_settled(user, &first));
      let other = claim("https://pictures.example.com/2.jpg");
      assert!(!pictures.is_settled(user, &other));
      pictures.remember(user, &other, &Err(ProviderPictureError::Unreachable.into()));
      assert!(pictures.is_settled(user, &other));
      age_failure(&pictures, user);
      assert!(!pictures.is_settled(user, &other));
    }

    #[test]
    fn a_failed_scim_photo_backs_off_too() {
      let pictures = ProviderPictures::new(1);
      let user = user(7);
      let broken = scim("https://pictures.example.com/broken.jpg");
      pictures.note_scim_managed(user);
      assert!(!pictures.is_settled(user, &broken));
      pictures.remember(user, &broken, &Err(ProviderPictureError::Status(404).into()));
      assert!(pictures.is_settled(user, &broken));
      assert!(!pictures.is_settled(user, &scim("https://pictures.example.com/fixed.jpg")));
      age_failure(&pictures, user);
      assert!(!pictures.is_settled(user, &broken));
    }

    #[test]
    fn past_the_limit_only_the_entry_noted_longest_ago_is_forgotten() {
      let pictures = ProviderPictures::new(1);
      let source = claim("https://pictures.example.com/1.jpg");
      let limit = i64::try_from(MAX_REMEMBERED).unwrap();
      for id in 1..=limit {
        pictures.remember(user(id), &source, &Ok(Imported::Stored));
      }
      if let Some(entry) = lock(&pictures.remembered).get_mut(&user(500)) {
        entry.noted_at = Instant::now().checked_sub(Duration::from_secs(60)).unwrap();
      }
      pictures.remember(user(limit + 1), &source, &Ok(Imported::Stored));
      let remembered = lock(&pictures.remembered);
      assert_eq!(remembered.len(), MAX_REMEMBERED);
      assert!(!remembered.contains_key(&user(500)));
      assert!(remembered.contains_key(&user(1)));
    }
  }

  mod precedence {
    use super::*;

    #[test]
    fn a_claim_of_a_person_scim_manages_is_settled_whatever_it_names() {
      let pictures = ProviderPictures::new(1);
      let user = user(7);
      let photo = scim("https://pictures.example.com/scim.jpg");
      pictures.note_scim_managed(user);
      pictures.remember(user, &photo, &Ok(Imported::Stored));
      assert!(pictures.is_settled(user, &claim("https://pictures.example.com/claim.jpg")));
      // A late claim result doesn't take SCIM's place.
      pictures.remember(
        user,
        &claim("https://pictures.example.com/claim.jpg"),
        &Ok(Imported::Stored),
      );
      assert!(pictures.is_settled(user, &claim("https://pictures.example.com/other.jpg")));
    }

    #[test]
    fn a_claim_left_to_scim_is_remembered_as_scim_managed() {
      let pictures = ProviderPictures::new(1);
      let user = user(7);
      let first = claim("https://pictures.example.com/1.jpg");
      pictures.remember(user, &first, &Ok(Imported::LeftToScim));
      assert!(pictures.is_settled(user, &claim("https://pictures.example.com/2.jpg")));
    }

    #[test]
    fn a_claim_never_displaces_a_scim_photo_in_the_worker() {
      let pictures = ProviderPictures::new(1);
      let user = user(7);
      let photo = scim("https://pictures.example.com/scim.jpg");
      assert_eq!(pictures.start_or_hand_over(user, &photo), Some(0));
      assert!(pictures.hand_over(user, &claim("https://pictures.example.com/claim.jpg")));
      assert!(pictures.is_current(user, 0));
      assert!(pictures.take_next(user).is_none());
    }
  }

  mod workers {
    use super::*;

    #[test]
    fn a_newer_source_supersedes_the_running_import_and_runs_next() {
      let pictures = ProviderPictures::new(1);
      let user = user(7);
      let (first, second, third) = (
        scim("https://pictures.example.com/1.jpg"),
        scim("https://pictures.example.com/2.jpg"),
        scim("https://pictures.example.com/3.jpg"),
      );
      assert_eq!(pictures.start_or_hand_over(user, &first), Some(0));
      assert_eq!(pictures.start_or_hand_over(user, &second), None);
      assert_eq!(pictures.start_or_hand_over(user, &third), None);
      assert!(!pictures.is_current(user, 0));
      // Only the newest runs next.
      let (next, generation) = pictures.take_next(user).unwrap();
      assert_eq!(next.source, third.source);
      assert!(pictures.is_current(user, generation));
      assert!(pictures.take_next(user).is_none());
      assert!(lock(&pictures.workers).is_empty());
    }

    #[test]
    fn the_running_source_again_changes_nothing() {
      let pictures = ProviderPictures::new(1);
      let user = user(7);
      let first = claim("https://pictures.example.com/1.jpg");
      assert_eq!(pictures.start_or_hand_over(user, &first), Some(0));
      // Every request of a signed-in member names the claim; it must not restart the import.
      assert!(pictures.is_settled(user, &first));
      assert!(pictures.is_current(user, 0));
    }

    #[test]
    fn a_removal_cancels_the_running_import_and_the_queued_one() {
      let pictures = ProviderPictures::new(1);
      let user = user(7);
      let first = scim("https://pictures.example.com/1.jpg");
      assert_eq!(pictures.start_or_hand_over(user, &first), Some(0));
      pictures.hand_over(user, &scim("https://pictures.example.com/2.jpg"));
      pictures.cancel(user);
      assert!(!pictures.is_current(user, 0));
      assert!(!pictures.is_current(user, 1));
      assert!(pictures.take_next(user).is_none());
    }
  }

  fn user(id: i64) -> UserId {
    UserId::from_snowflake(teamcraft_proto::Snowflake::new(id).unwrap())
  }

  fn claim(url: &str) -> Job {
    Job {
      origin: Origin::Claim,
      source: PictureSource::Url(url.into()),
    }
  }

  fn scim(value: &str) -> Job {
    Job {
      origin: Origin::Scim,
      source: PictureSource::from_scim(value),
    }
  }

  fn age_failure(pictures: &ProviderPictures, user: UserId) {
    if let Some(entry) = lock(&pictures.remembered).get_mut(&user) {
      entry.maybe_failed_at = Instant::now().checked_sub(RETRY_AFTER + Duration::from_secs(1));
    }
  }
}
