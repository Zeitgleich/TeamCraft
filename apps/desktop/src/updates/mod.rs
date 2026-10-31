//! Desktop updates (docs/updates.md): checks the feed in the background, downloads and verifies
//! updates, and applies them on restart or quit, never during a call. What is allowed comes from
//! `teamcraft_client_api::updates`; this module does the I/O.
//!
//! The device's preference and the last verified policy and server minimum are kept in
//! `updates.json`, so the organization's rules apply before the next sign-in as well.

mod feed;
#[cfg(target_os = "linux")]
mod package_kit;

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State, Url};
use tauri_plugin_updater::{Update, UpdaterExt};
use teamcraft_client_api::updates::{
  CheckPlan, ClientVersion, Delivery, InstallKind, InstallPhase, ManualUpdate, PackageManager,
  PackageManagerHint, PackageSource, PlanInputs, UpdatePlan, UpdatePreference, UpdateState, UpdateStatus,
  is_offered, missing_version, next_check_delay, plan, required_version,
};
use teamcraft_proto::{ClientCompatibilityResponse, ClientUpdatePolicy};
use tokio::sync::Notify;

use crate::auth::{AuthService, KeyringStore};
use crate::calls::CallHost;
use crate::core::commands::CoreHost;

pub use feed::BUILD_VERSION;
#[cfg(target_os = "linux")]
pub use feed::install_kind;

const FILE_NAME: &str = "updates.json";
/// Tells the webview to read the status again.
pub const UPDATE_CHANGED_EVENT: &str = "update-changed";
/// How often a deferred download (a call was running) is retried.
const RETRY_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// Machine-wide switch for IT and packagers (docs/updates.md §4.4).
const DISABLE_VARIABLE: &str = "TEAMCRAFT_DISABLE_UPDATES";
const COMPATIBILITY_PATH: &str = "/api/v1/client-compatibility";

/// The shell's update machinery, shared by the background task, the commands and the exit hook.
pub struct UpdateHost {
  install: InstallKind,
  maybe_current: Option<ClientVersion>,
  has_update_key: bool,
  inner: Mutex<Inner>,
  wake: Notify,
}

/// What is kept across launches.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct SavedUpdates {
  preference: UpdatePreference,
  #[serde(rename = "policy", skip_serializing_if = "Option::is_none")]
  maybe_policy: Option<ClientUpdatePolicy>,
  #[serde(rename = "server_minimum", skip_serializing_if = "Option::is_none")]
  maybe_server_minimum: Option<String>,
  /// The version whose hint the user dismissed (`Dismissal::ForVersion`).
  #[serde(rename = "dismissed_version", skip_serializing_if = "Option::is_none")]
  maybe_dismissed_version: Option<String>,
}

struct Inner {
  saved: SavedUpdates,
  state: UpdateState,
  /// Found and accepted by the version check, not downloaded yet.
  maybe_found: Option<Update>,
  /// Downloaded and verified: installed on restart or quit.
  maybe_ready: Option<(Update, Vec<u8>)>,
  /// How the user installs an update the app can't (deb, rpm, Flatpak, a read-only AppImage).
  maybe_manual: Option<ManualUpdate>,
}

#[derive(Debug, Serialize)]
pub struct UpdateCommandError {
  kind: &'static str,
  message: String,
}

impl UpdateHost {
  pub fn load(app: &AppHandle) -> Self {
    let saved = path(app)
      .and_then(|path| std::fs::read_to_string(path).ok())
      .and_then(|text| serde_json::from_str(&text).ok())
      .unwrap_or_default();
    let host = Self {
      install: feed::install_kind(),
      maybe_current: feed::build_version(),
      has_update_key: has_update_key(app),
      inner: Mutex::new(Inner {
        saved,
        state: UpdateState::Idle {
          maybe_checked_at: None,
        },
        maybe_found: None,
        maybe_ready: None,
        maybe_manual: None,
      }),
      wake: Notify::new(),
    };
    // Settings shows why nothing is checked right away, not only after the first check.
    if let UpdatePlan::Off(reason) = host.plan() {
      host.lock().state = UpdateState::Off { reason };
    }
    host
  }

  /// Checks on start (after a random delay) and about daily, until the app ends.
  pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
      let host = app.state::<Arc<UpdateHost>>().inner().clone();
      let mut next_check = tokio::time::Instant::now() + next_check_delay(true, jitter());
      loop {
        let wait = next_check
          .saturating_duration_since(tokio::time::Instant::now())
          .min(RETRY_INTERVAL);
        let is_woken = tokio::select! {
          () = tokio::time::sleep(wait) => false,
          () = host.wake.notified() => true,
        };
        if is_woken || tokio::time::Instant::now() >= next_check {
          host.check(&app).await;
          next_check = tokio::time::Instant::now() + next_check_delay(false, jitter());
        } else {
          host.resume_deferred_download(&app).await;
        }
      }
    });
  }

  pub fn status(&self, app: &AppHandle) -> UpdateStatus {
    let inner = self.lock();
    let maybe_required = required_version(
      inner.saved.maybe_server_minimum.as_deref(),
      inner
        .saved
        .maybe_policy
        .as_ref()
        .and_then(|policy| policy.maybe_minimum_version.as_deref()),
    );
    UpdateStatus {
      current_version: BUILD_VERSION.to_owned(),
      install: self.install,
      preference: inner.saved.preference,
      state: inner.state.clone(),
      maybe_required_version: self
        .maybe_current
        .as_ref()
        .and_then(|current| missing_version(current, maybe_required.as_ref())),
      is_in_call: is_in_call(app),
      maybe_manual: inner.maybe_manual.clone(),
      maybe_dismissed_version: inner.saved.maybe_dismissed_version.clone(),
    }
  }

  /// Installs a downloaded update while the app quits, if the user wants automatic updates and no
  /// call is running (docs/updates.md §4.3). Windows starts the installer and exits here.
  pub fn install_on_exit(&self, app: &AppHandle) {
    if is_in_call(app) {
      return;
    }
    let maybe_ready = {
      let mut inner = self.lock();
      if inner.saved.preference != UpdatePreference::Automatic {
        return;
      }
      inner.maybe_ready.take()
    };
    if let Some((update, bytes)) = maybe_ready {
      tracing::info!(version = %update.version, "installing the update on quit");
      if let Err(error) = update.restart_after_install(false).install(bytes) {
        tracing::warn!(%error, "could not install the update on quit");
      }
    }
  }

  async fn check(&self, app: &AppHandle) {
    self.refresh_rules(app).await;
    let check_plan = match self.plan() {
      UpdatePlan::Off(reason) => {
        self.set_state(app, UpdateState::Off { reason });
        return;
      },
      UpdatePlan::Check(check_plan) => check_plan,
    };
    if self.is_busy() {
      return;
    }
    self.set_state(app, UpdateState::Checking);
    let result = match check_plan.delivery {
      Delivery::SelfUpdate { is_automatic } => self.check_self_update(app, &check_plan, is_automatic).await,
      Delivery::PackageManager(manager) => self.check_package(app, &check_plan, manager).await,
    };
    if let Err(message) = result {
      tracing::warn!(%message, "update check failed");
      self.set_state(app, UpdateState::Failed { message });
    }
  }

  /// The policy the core verified and the server's minimum, kept for the next launch.
  async fn refresh_rules(&self, app: &AppHandle) {
    let maybe_policy = match app.state::<CoreHost>().current() {
      Ok(runtime) => runtime.client_update_policy().await,
      Err(_) => None,
    };
    let maybe_minimum = server_minimum(app).await;
    let mut inner = self.lock();
    let before = inner.saved.clone();
    if let Some(policy) = maybe_policy {
      inner.saved.maybe_policy = Some(policy);
    }
    if let Some(minimum) = maybe_minimum {
      inner.saved.maybe_server_minimum = Some(minimum);
    }
    if inner.saved != before {
      save(app, &inner.saved);
    }
  }

  fn plan(&self) -> UpdatePlan {
    let inner = self.lock();
    plan(PlanInputs {
      install: self.install,
      preference: inner.saved.preference,
      maybe_policy: inner.saved.maybe_policy.as_ref(),
      is_disabled_on_machine: is_disabled_on_machine(),
      has_update_key: self.has_update_key,
    })
  }

  async fn check_self_update(
    &self,
    app: &AppHandle,
    check_plan: &CheckPlan,
    is_automatic: bool,
  ) -> Result<(), String> {
    let Some(current) = self.maybe_current.clone() else {
      return Err("this build has no readable version".to_owned());
    };
    let url = Url::parse(&feed::manifest_url(check_plan)).map_err(|error| error.to_string())?;
    let maybe_pinned = check_plan.maybe_pinned.clone();
    let updater = app
      .updater_builder()
      .endpoints(vec![url])
      .map_err(|error| error.to_string())?
      // The running version is the tag's (a candidate knows it is one), not the crate version the
      // updater sees.
      .version_comparator(move |_, release| {
        ClientVersion::parse(&release.version.to_string())
          .is_ok_and(|remote| is_offered(&current, &remote, maybe_pinned.as_ref()))
      })
      .build()
      .map_err(|error| error.to_string())?;
    let Some(update) = updater.check().await.map_err(|error| error.to_string())? else {
      self.set_state(app, idle_now());
      return Ok(());
    };
    let version = update.version.clone();
    let maybe_notes_url = release_page(update.body.as_deref());
    tracing::info!(%version, "update available");
    // An AppImage in a place the app can't write (#731): the user downloads the new one.
    let is_replaceable = self.install != InstallKind::AppImage || can_replace_app_image();
    {
      let mut inner = self.lock();
      inner.maybe_manual = (!is_replaceable).then(|| ManualUpdate::Download {
        url: update.download_url.to_string(),
        maybe_repository_setup_url: None,
      });
      inner.maybe_found = is_replaceable.then_some(update);
    }
    self.set_state(
      app,
      UpdateState::Available {
        version,
        maybe_notes_url,
      },
    );
    if is_replaceable && is_automatic && !is_in_call(app) {
      self.download(app).await?;
    }
    Ok(())
  }

  /// deb, rpm and Flatpak: reads the feed like the other installs and tells the user how to get a
  /// new version, which depends on where the package came from (#731).
  async fn check_package(
    &self,
    app: &AppHandle,
    check_plan: &CheckPlan,
    manager: PackageManager,
  ) -> Result<(), String> {
    let Some(current) = self.maybe_current.clone() else {
      return Err("this build has no readable version".to_owned());
    };
    let source = match tokio::task::spawn_blocking(move || feed::package_source(manager)).await {
      Ok(source) => source,
      Err(error) => {
        tracing::debug!(%error, "could not read the package sources");
        PackageSource::File
      },
    };
    // Set before the feed answers, so "too old" can say what to do even when the check fails.
    let hint = manual_update(manager, source, None, check_plan).await;
    self.lock().maybe_manual = Some(hint);
    let summary = fetch_manifest(&feed::manifest_url(check_plan)).await?;
    let remote = ClientVersion::parse(&summary.version).map_err(|error| error.to_string())?;
    let state = if is_offered(&current, &remote, check_plan.maybe_pinned.as_ref()) {
      let maybe_notes_url = release_page(summary.notes.as_deref());
      let target = DownloadTarget {
        version: &remote,
        maybe_notes_url: maybe_notes_url.as_deref(),
      };
      let hint = manual_update(manager, source, Some(target), check_plan).await;
      self.lock().maybe_manual = Some(hint);
      UpdateState::Available {
        version: summary.version,
        maybe_notes_url,
      }
    } else {
      idle_now()
    };
    self.set_state(app, state);
    Ok(())
  }

  /// Downloads and verifies the found update; it is installed on restart or quit.
  async fn download(&self, app: &AppHandle) -> Result<(), String> {
    let Some(update) = self.lock().maybe_found.clone() else {
      return Err("no update to download".to_owned());
    };
    let version = update.version.clone();
    let maybe_notes_url = release_page(update.body.as_deref());
    self.set_state(
      app,
      UpdateState::Downloading {
        version: version.clone(),
        maybe_percent: None,
      },
    );
    let progress_app = app.clone();
    let progress_version = version.clone();
    let mut received: u64 = 0;
    let mut last_percent = None;
    let bytes = update
      .download(
        move |chunk, maybe_total| {
          received = received.saturating_add(chunk as u64);
          let maybe_percent = maybe_total.filter(|total| *total > 0).map(|total| {
            u8::try_from(received.saturating_mul(100) / total)
              .unwrap_or(100)
              .min(100)
          });
          if maybe_percent != last_percent {
            last_percent = maybe_percent;
            progress_app.state::<Arc<UpdateHost>>().set_state(
              &progress_app,
              UpdateState::Downloading {
                version: progress_version.clone(),
                maybe_percent,
              },
            );
          }
        },
        || {},
      )
      .await
      .map_err(|error| error.to_string())?;
    {
      let mut inner = self.lock();
      inner.maybe_found = None;
      inner.maybe_ready = Some((update, bytes));
    }
    tracing::info!(%version, "update downloaded and verified");
    self.set_state(
      app,
      UpdateState::Ready {
        version,
        maybe_notes_url,
      },
    );
    Ok(())
  }

  /// An automatic download that waited for a call to end.
  async fn resume_deferred_download(&self, app: &AppHandle) {
    let is_due = {
      let inner = self.lock();
      inner.maybe_found.is_some()
        && inner.saved.preference == UpdatePreference::Automatic
        && matches!(inner.state, UpdateState::Available { .. })
    };
    if is_due
      && !is_in_call(app)
      && let Err(message) = self.download(app).await
    {
      tracing::warn!(%message, "update download failed");
      self.set_state(app, UpdateState::Failed { message });
    }
  }

  fn is_busy(&self) -> bool {
    matches!(
      self.lock().state,
      UpdateState::Downloading { .. } | UpdateState::Installing { .. } | UpdateState::Ready { .. }
    )
  }

  fn set_state(&self, app: &AppHandle, state: UpdateState) {
    {
      let mut inner = self.lock();
      if inner.state == state {
        return;
      }
      inner.state = state;
    }
    if let Err(error) = app.emit(UPDATE_CHANGED_EVENT, ()) {
      tracing::debug!(%error, "could not notify the webview");
    }
  }

  fn lock(&self) -> MutexGuard<'_, Inner> {
    self.inner.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

#[tauri::command(rename_all = "snake_case")]
pub fn update_status(app: AppHandle, host: State<'_, Arc<UpdateHost>>) -> UpdateStatus {
  host.status(&app)
}

/// "Check now".
#[tauri::command(rename_all = "snake_case")]
pub fn update_check(host: State<'_, Arc<UpdateHost>>) {
  host.wake.notify_one();
}

#[tauri::command(rename_all = "snake_case")]
pub fn update_set_preference(app: AppHandle, host: State<'_, Arc<UpdateHost>>, preference: UpdatePreference) {
  {
    let mut inner = host.lock();
    inner.saved.preference = preference;
    save(&app, &inner.saved);
  }
  host.wake.notify_one();
}

/// Notify-only: downloads the update the user asked for.
#[tauri::command(rename_all = "snake_case")]
pub async fn update_download(
  app: AppHandle,
  host: State<'_, Arc<UpdateHost>>,
) -> Result<(), UpdateCommandError> {
  if is_in_call(&app) {
    return Err(in_call());
  }
  let host = host.inner().clone();
  let offer = host.lock().state.clone();
  host.download(&app).await.map_err(|message| {
    // The banner's download failed: the offer stays, with the error next to the button that tries
    // again (#1044). The found update is still there to download.
    let state = if matches!(offer, UpdateState::Available { .. }) {
      offer
    } else {
      UpdateState::Failed {
        message: message.clone(),
      }
    };
    host.set_state(&app, state);
    UpdateCommandError {
      kind: "failed",
      message,
    }
  })
}

/// "Restart to update": installs the downloaded update and starts the new version. Refused during
/// a call. After a package manager update there is nothing to install, only to restart.
#[tauri::command(rename_all = "snake_case")]
pub fn update_restart(app: AppHandle, host: State<'_, Arc<UpdateHost>>) -> Result<(), UpdateCommandError> {
  if is_in_call(&app) {
    return Err(in_call());
  }
  if !matches!(host.lock().state, UpdateState::Ready { .. }) {
    return Err(UpdateCommandError {
      kind: "not_ready",
      message: "no update is ready".to_owned(),
    });
  }
  let maybe_ready = host.lock().maybe_ready.take();
  if let Some((update, bytes)) = maybe_ready
    && let Err(error) = update.install(bytes)
  {
    let message = error.to_string();
    host.set_state(
      &app,
      UpdateState::Failed {
        message: message.clone(),
      },
    );
    return Err(UpdateCommandError {
      kind: "failed",
      message,
    });
  }
  app.restart()
}

/// Hides the hint for `version` for good (`Dismissal::ForVersion`): the app can't install it, and
/// the user knows (#731).
#[tauri::command(rename_all = "snake_case")]
pub fn update_dismiss(app: AppHandle, host: State<'_, Arc<UpdateHost>>, version: String) {
  {
    let mut inner = host.lock();
    if inner.saved.maybe_dismissed_version.as_ref() == Some(&version) {
      return;
    }
    inner.saved.maybe_dismissed_version = Some(version);
    save(&app, &inner.saved);
  }
  if let Err(error) = app.emit(UPDATE_CHANGED_EVENT, ()) {
    tracing::debug!(%error, "could not notify the webview");
  }
}

/// deb/rpm: asks PackageKit to install the update (docs/updates.md §4.5).
#[tauri::command(rename_all = "snake_case")]
pub async fn update_with_package_manager(
  app: AppHandle,
  host: State<'_, Arc<UpdateHost>>,
) -> Result<(), UpdateCommandError> {
  let host = host.inner().clone();
  let offer = host.lock().state.clone();
  let UpdateState::Available { version, .. } = offer.clone() else {
    return Err(UpdateCommandError {
      kind: "not_ready",
      message: "no update is available".to_owned(),
    });
  };
  let set_phase = {
    let (app, host, version) = (app.clone(), host.clone(), version.clone());
    move |phase| {
      host.set_state(
        &app,
        UpdateState::Installing {
          version: version.clone(),
          phase,
        },
      );
    }
  };
  set_phase(InstallPhase::Running);
  match install_package(set_phase).await {
    Ok(()) => {
      tracing::info!(%version, "package updated");
      host.set_state(
        &app,
        UpdateState::Ready {
          version,
          maybe_notes_url: None,
        },
      );
      Ok(())
    },
    Err(message) => {
      tracing::warn!(%message, "package update failed");
      // The offer stays, so the banner shows the error next to the button that tries again (#1044).
      host.set_state(&app, offer);
      Err(UpdateCommandError {
        kind: "failed",
        message,
      })
    },
  }
}

#[cfg(target_os = "linux")]
async fn install_package(on_phase: impl Fn(InstallPhase)) -> Result<(), String> {
  package_kit::update_teamcraft(on_phase)
    .await
    .map(|_| ())
    .map_err(|error| error.to_string())
}

#[cfg(not(target_os = "linux"))]
async fn install_package(_on_phase: impl Fn(InstallPhase)) -> Result<(), String> {
  Err("package managers are only used on Linux".to_owned())
}

#[cfg(target_os = "linux")]
async fn has_package_kit() -> bool {
  package_kit::is_available().await
}

#[cfg(not(target_os = "linux"))]
async fn has_package_kit() -> bool {
  false
}

/// The version a hint points to, once the feed named one.
#[derive(Clone, Copy)]
struct DownloadTarget<'a> {
  version: &'a ClientVersion,
  maybe_notes_url: Option<&'a str>,
}

/// What the user does to get a new version onto a deb, rpm or Flatpak install: the package
/// manager's command (and PackageKit) from a repository, or the new file and how to add the
/// repository when the package was installed from a file.
async fn manual_update(
  manager: PackageManager,
  source: PackageSource,
  maybe_target: Option<DownloadTarget<'_>>,
  check_plan: &CheckPlan,
) -> ManualUpdate {
  match source {
    PackageSource::Repository => ManualUpdate::PackageManager(PackageManagerHint {
      manager,
      command: manager.command(),
      has_package_kit: manager.is_package_kit_managed() && has_package_kit().await,
    }),
    PackageSource::File => {
      let url = match maybe_target {
        // A mirrored feed (policy): its release page, not GitHub's assets.
        Some(target) if check_plan.maybe_feed_url.is_some() => target
          .maybe_notes_url
          .map_or_else(feed::releases_url, str::to_owned),
        Some(target) => feed::package_download_url(manager, target.version),
        None => feed::releases_url(),
      };
      ManualUpdate::Download {
        url,
        maybe_repository_setup_url: Some(feed::repository_setup_url()),
      }
    },
  }
}

/// AppImage: whether the updater can replace the file. It moves the running AppImage aside and
/// writes the new one next to it, so its directory must be writable (not, say, `/opt` owned by
/// root or a read-only mount).
#[cfg(target_os = "linux")]
fn can_replace_app_image() -> bool {
  let Some(path) = std::env::var_os("APPIMAGE").map(PathBuf::from) else {
    return false;
  };
  let Some(dir) = path.parent() else {
    return false;
  };
  let probe = dir.join(format!(".teamcraft-update-probe-{}", std::process::id()));
  match std::fs::OpenOptions::new()
    .write(true)
    .create_new(true)
    .open(&probe)
  {
    Ok(_) => {
      if let Err(error) = std::fs::remove_file(&probe) {
        // Harmless: an empty hidden file next to the AppImage.
        tracing::debug!(%error, "could not remove the write probe");
      }
      true
    },
    Err(error) => {
      tracing::info!(%error, "the AppImage can't be replaced; the update is offered as a download");
      false
    },
  }
}

#[cfg(not(target_os = "linux"))]
fn can_replace_app_image() -> bool {
  false
}

/// The server's minimum client version, if it answers (older servers don't know the endpoint).
async fn server_minimum(app: &AppHandle) -> Option<String> {
  let service = app.state::<Arc<AuthService<KeyringStore>>>();
  let response = service
    .public_get(COMPATIBILITY_PATH)
    .await
    .inspect_err(|error| tracing::debug!(%error, "no client compatibility from the server"))
    .ok()?;
  if response.status != 200 {
    return None;
  }
  serde_json::from_str::<ClientCompatibilityResponse>(&response.body)
    .ok()
    .map(|compatibility| compatibility.minimum_client_version)
}

async fn fetch_manifest(url: &str) -> Result<feed::ManifestSummary, String> {
  let response = reqwest::Client::new()
    .get(url)
    .header(reqwest::header::ACCEPT, "application/json")
    .timeout(Duration::from_secs(30))
    .send()
    .await
    .map_err(|error| error.to_string())?
    .error_for_status()
    .map_err(|error| error.to_string())?;
  response.json().await.map_err(|error| error.to_string())
}

fn is_in_call(app: &AppHandle) -> bool {
  app
    .try_state::<Arc<CallHost>>()
    .is_some_and(|calls| calls.snapshot().maybe_active.is_some())
}

fn is_disabled_on_machine() -> bool {
  std::env::var(DISABLE_VARIABLE).is_ok_and(|value| matches!(value.trim(), "1" | "true" | "yes"))
}

/// The build carries the updater's public key (`plugins.updater.pubkey`); without it nothing could
/// be verified (docs/updates.md §3.3).
fn has_update_key(app: &AppHandle) -> bool {
  app
    .config()
    .plugins
    .0
    .get("updater")
    .and_then(|updater| updater.get("pubkey"))
    .and_then(serde_json::Value::as_str)
    .is_some_and(|key| !key.trim().is_empty())
}

/// The manifest's `notes` is the release page; anything that isn't a web link is not shown.
fn release_page(maybe_notes: Option<&str>) -> Option<String> {
  maybe_notes
    .map(str::trim)
    .filter(|notes| notes.starts_with("https://") && !notes.contains(char::is_whitespace))
    .map(str::to_owned)
}

fn idle_now() -> UpdateState {
  let now = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .ok()
    .and_then(|since| i64::try_from(since.as_millis()).ok());
  UpdateState::Idle {
    maybe_checked_at: now,
  }
}

/// A number in `[0, 1)` for spreading checks; not security relevant, so the standard library's
/// randomly seeded hasher is enough.
fn jitter() -> f64 {
  let random = RandomState::new().hash_one(SystemTime::now());
  (random >> 11) as f64 / (1_u64 << 53) as f64
}

fn in_call() -> UpdateCommandError {
  UpdateCommandError {
    kind: "in_call",
    message: "not during a call".to_owned(),
  }
}

fn save(app: &AppHandle, saved: &SavedUpdates) {
  let Some(path) = path(app) else {
    return;
  };
  let result = serde_json::to_string_pretty(saved)
    .map_err(|error| error.to_string())
    .and_then(|text| {
      if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
      }
      std::fs::write(&path, text).map_err(|error| error.to_string())
    });
  if let Err(error) = result {
    tracing::warn!(%error, "could not save the update settings");
  }
}

fn path(app: &AppHandle) -> Option<PathBuf> {
  app.path().app_config_dir().ok().map(|dir| dir.join(FILE_NAME))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_web_links_count_as_release_pages() {
    assert_eq!(
      release_page(Some(" https://github.com/Example/TeamCraft/releases/tag/v1.0.1 ")),
      Some("https://github.com/Example/TeamCraft/releases/tag/v1.0.1".to_owned())
    );
    assert_eq!(release_page(Some("Fixes a crash")), None);
    assert_eq!(release_page(Some("http://example.com")), None);
    assert_eq!(release_page(Some("https://example.com <script>")), None);
    assert_eq!(release_page(None), None);
  }

  #[test]
  fn jitter_stays_in_range() {
    for _ in 0..100 {
      let value = jitter();
      assert!((0.0..1.0).contains(&value), "{value}");
    }
  }

  #[test]
  fn saved_settings_survive_missing_fields() {
    let saved: SavedUpdates = serde_json::from_str(r#"{"preference":"notify_only"}"#).unwrap_or_default();
    assert_eq!(saved.preference, UpdatePreference::NotifyOnly);
    assert_eq!(saved.maybe_policy, None);
    assert_eq!(saved.maybe_dismissed_version, None);
    let dismissed: SavedUpdates =
      serde_json::from_str(r#"{"dismissed_version":"1.0.8"}"#).unwrap_or_default();
    assert_eq!(dismissed.maybe_dismissed_version.as_deref(), Some("1.0.8"));
    let empty: SavedUpdates = serde_json::from_str("{}").unwrap_or_default();
    assert_eq!(empty, SavedUpdates::default());
  }
}
