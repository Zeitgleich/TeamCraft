//! Desktop update decisions (docs/updates.md): which versions count as updates, what the
//! organization's signed policy and the device's preference allow, and what the UI shows. Pure
//! logic; the desktop shell does the checking, downloading and installing.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use teamcraft_proto::{ClientUpdateMode, ClientUpdatePolicy, UpdateChannel};

use crate::model::TimestampMs;

/// First check after start: between these (docs/updates.md §4.1).
const FIRST_CHECK_MIN: Duration = Duration::from_secs(60);
const FIRST_CHECK_MAX: Duration = Duration::from_secs(10 * 60);
/// Later checks: a day plus or minus this.
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const CHECK_JITTER: Duration = Duration::from_secs(2 * 60 * 60);
/// The name of the deb and rpm packages.
pub const LINUX_PACKAGE_NAME: &str = "team-craft";
/// The Flatpak's application ID.
pub const FLATPAK_APP_ID: &str = "de.zeitgleich.teamcraft";
/// How the bundler starts the names of the deb and rpm files of a release (the product name).
const LINUX_PACKAGE_FILE_PREFIX: &str = "TeamCraft";

/// How this copy of the app was installed, which decides how it updates (docs/updates.md §2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallKind {
  WindowsNsis,
  WindowsMsi,
  MacApp,
  AppImage,
  Deb,
  Rpm,
  Flatpak,
  /// Development builds and anything the bundler didn't mark.
  Unpackaged,
}

/// The device's own choice (Settings → Updates).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdatePreference {
  /// Download in the background, install on restart or quit.
  #[default]
  Automatic,
  /// Say that an update exists; download when the user asks.
  NotifyOnly,
  Off,
}

/// What updates an install that doesn't replace itself: the system package manager of a deb or rpm
/// install, or Flatpak.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageManager {
  Apt,
  Dnf,
  Flatpak,
}

/// Where a deb or rpm install came from, which decides what the update hint says (#731).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageSource {
  /// TeamCraft's APT or RPM repository is configured: the package manager brings new versions.
  Repository,
  /// Installed from a downloaded file: the package manager doesn't know where new versions are.
  File,
}

/// A release version: `1.2.3`, `1.2.3-rc.1`, `1.2.3-beta.2`. Ordered by semantic versioning, so a
/// candidate comes before its release.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientVersion(semver::Version);

/// Why nothing is checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateBlocker {
  /// The organization's policy turned self-updates off.
  Policy,
  /// `TEAMCRAFT_DISABLE_UPDATES` on this machine.
  Machine,
  /// The user chose "Off".
  Preference,
  /// This build has no update key, so it couldn't verify an update.
  NotConfigured,
  /// A development build.
  Unpackaged,
}

/// How a found update gets installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
  /// The app replaces itself; `is_automatic` downloads without asking.
  SelfUpdate { is_automatic: bool },
  /// The package manager or Flatpak installs it; the app only tells (and may ask PackageKit).
  PackageManager(PackageManager),
}

/// What the shell does: nothing, or check this feed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdatePlan {
  Off(UpdateBlocker),
  Check(CheckPlan),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckPlan {
  pub channel: UpdateChannel,
  /// Only this version is offered.
  pub maybe_pinned: Option<ClientVersion>,
  /// A mirror of the manifest that replaces the channel URLs (https only).
  pub maybe_feed_url: Option<String>,
  pub delivery: Delivery,
}

/// Everything [`plan`] decides from.
#[derive(Clone, Copy, Debug)]
pub struct PlanInputs<'a> {
  pub install: InstallKind,
  pub preference: UpdatePreference,
  /// The last policy this device verified; `None` before the first sign-in.
  pub maybe_policy: Option<&'a ClientUpdatePolicy>,
  pub is_disabled_on_machine: bool,
  /// The build carries the updater's public key.
  pub has_update_key: bool,
}

/// What the shell reports to the UI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateStatus {
  /// The running version (the release tag's version for candidates).
  pub current_version: String,
  pub install: InstallKind,
  pub preference: UpdatePreference,
  pub state: UpdateState,
  /// Set when the running version is older than the organization or server accepts.
  #[serde(rename = "required_version", default, skip_serializing_if = "Option::is_none")]
  pub maybe_required_version: Option<String>,
  /// A call is running: nothing downloads, nothing restarts.
  #[serde(default)]
  pub is_in_call: bool,
  /// Linux installs the app can't update itself (deb, rpm, Flatpak, an AppImage it can't replace):
  /// how the user gets the new version.
  #[serde(rename = "manual", default, skip_serializing_if = "Option::is_none")]
  pub maybe_manual: Option<ManualUpdate>,
  /// The version whose hint the user dismissed ([`Dismissal::ForVersion`]); kept across launches.
  #[serde(
    rename = "dismissed_version",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_dismissed_version: Option<String>,
}

/// How a new version gets onto a Linux install that doesn't replace itself (docs/updates.md §4.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ManualUpdate {
  /// deb/rpm from TeamCraft's repository, or Flatpak: the command to run, and PackageKit's
  /// "Update now" where it runs.
  PackageManager(PackageManagerHint),
  /// A deb/rpm installed from a file, or an AppImage in a place the app can't write: download the
  /// new file. `maybe_repository_setup_url` explains how to add the repository (deb/rpm).
  Download {
    url: String,
    #[serde(
      rename = "repository_setup_url",
      default,
      skip_serializing_if = "Option::is_none"
    )]
    maybe_repository_setup_url: Option<String>,
  },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageManagerHint {
  pub manager: PackageManager,
  pub command: String,
  pub has_package_kit: bool,
}

/// Where the update machinery is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UpdateState {
  Off {
    reason: UpdateBlocker,
  },
  /// Up to date (or not checked yet).
  Idle {
    #[serde(rename = "checked_at", default, skip_serializing_if = "Option::is_none")]
    maybe_checked_at: Option<TimestampMs>,
  },
  Checking,
  /// Found, not downloaded: notify-only, a deb/rpm install, or waiting for a call to end.
  Available {
    version: String,
    #[serde(rename = "notes_url", default, skip_serializing_if = "Option::is_none")]
    maybe_notes_url: Option<String>,
  },
  Downloading {
    version: String,
    #[serde(rename = "percent", default, skip_serializing_if = "Option::is_none")]
    maybe_percent: Option<u8>,
  },
  /// PackageKit is installing the package.
  Installing {
    version: String,
  },
  /// Downloaded and verified (or installed by the package manager): a restart applies it.
  Ready {
    version: String,
    #[serde(rename = "notes_url", default, skip_serializing_if = "Option::is_none")]
    maybe_notes_url: Option<String>,
  },
  Failed {
    message: String,
  },
}

/// The banner the app shows, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateBanner {
  /// "TeamCraft X is ready — Restart to update". `can_restart` is false during a call.
  Ready {
    version: String,
    maybe_notes_url: Option<String>,
    can_restart: bool,
  },
  /// "TeamCraft X is available" with a download button or the package manager's command.
  Available {
    version: String,
    maybe_notes_url: Option<String>,
    action: AvailableAction,
  },
  /// "This version is too old for your organization" — stays until updated.
  TooOld {
    required_version: String,
    action: TooOldAction,
  },
}

/// Whether and how the user can hide the banner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dismissal {
  /// It stays until acted on: the version is too old, or a self-installed update waits for the
  /// restart that applies it (#729).
  Never,
  /// "Later" hides it until the app starts again (an update the app installs itself, not
  /// downloaded yet).
  UntilRelaunch,
  /// Hidden for this version, also after a restart: the app can't install it itself (deb, rpm,
  /// Flatpak, a read-only AppImage), so a hint the user has seen must not nag (#731).
  ForVersion(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AvailableAction {
  /// Download now (disabled during a call).
  Download { can_download: bool },
  /// The app can't install it: what the user does instead.
  Manual(ManualUpdate),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TooOldAction {
  /// The app can update itself; what the user can do about it right now.
  SelfUpdate(SelfUpdateStep),
  Manual(ManualUpdate),
  /// Updates are off here (policy, machine, Flatpak): IT or the store has to do it.
  Elsewhere,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelfUpdateStep {
  /// Downloaded: restart (not during a call).
  Restart { can_restart: bool },
  /// Found, not downloaded (notify only, or waiting for a call to end).
  Download { can_download: bool },
  /// Checking, downloading, or not found yet: nothing to click.
  Wait,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UpdateError {
  #[error("not a version: {0}")]
  InvalidVersion(String),
}

impl ClientVersion {
  /// Parses `1.2.3` or `v1.2.3-rc.1`.
  pub fn parse(text: &str) -> Result<Self, UpdateError> {
    let trimmed = text.trim();
    let trimmed = trimmed.strip_prefix('v').unwrap_or(trimmed);
    semver::Version::parse(trimmed)
      .map(Self)
      .map_err(|_| UpdateError::InvalidVersion(text.to_owned()))
  }

  pub fn is_prerelease(&self) -> bool {
    !self.0.pre.is_empty()
  }
}

impl fmt::Display for ClientVersion {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    self.0.fmt(f)
  }
}

impl InstallKind {
  /// Whether the app can replace itself.
  pub fn updates_itself(self) -> bool {
    matches!(
      self,
      Self::WindowsNsis | Self::WindowsMsi | Self::MacApp | Self::AppImage
    )
  }

  pub fn maybe_package_manager(self) -> Option<PackageManager> {
    match self {
      Self::Deb => Some(PackageManager::Apt),
      Self::Rpm => Some(PackageManager::Dnf),
      Self::Flatpak => Some(PackageManager::Flatpak),
      _ => None,
    }
  }
}

impl PackageManager {
  /// The command a user runs to update TeamCraft alone.
  pub fn command(self) -> String {
    match self {
      Self::Apt => format!("sudo apt update && sudo apt install --only-upgrade {LINUX_PACKAGE_NAME}"),
      Self::Dnf => format!("sudo dnf upgrade --refresh {LINUX_PACKAGE_NAME}"),
      Self::Flatpak => format!("flatpak update {FLATPAK_APP_ID}"),
    }
  }

  /// Whether PackageKit can install the update (deb and rpm; Flatpak has its own tools).
  pub fn is_package_kit_managed(self) -> bool {
    matches!(self, Self::Apt | Self::Dnf)
  }
}

/// The release asset of a deb or rpm for `version` on `arch` (Rust's `std::env::consts::ARCH`), as
/// the release names it: `TeamCraft_1.0.8_amd64.deb`, `TeamCraft-1.0.8-1.x86_64.rpm`. `None` for
/// candidates (their package versions are mapped, scripts/linux-package-version.sh, so the release
/// page is the safer link), for Flatpak and for architectures without packages.
pub fn linux_package_file(manager: PackageManager, version: &ClientVersion, arch: &str) -> Option<String> {
  if version.is_prerelease() {
    return None;
  }
  match (manager, arch) {
    (PackageManager::Apt, "x86_64") => Some(format!("{LINUX_PACKAGE_FILE_PREFIX}_{version}_amd64.deb")),
    (PackageManager::Apt, "aarch64") => Some(format!("{LINUX_PACKAGE_FILE_PREFIX}_{version}_arm64.deb")),
    (PackageManager::Dnf, "x86_64" | "aarch64") => {
      Some(format!("{LINUX_PACKAGE_FILE_PREFIX}-{version}-1.{arch}.rpm"))
    },
    _ => None,
  }
}

/// Whether a dnf or zypper `.repo` file enables TeamCraft's repository: a section named
/// `teamcraft…` (as docs/updates.md §5 writes it) or one whose URL starts with `repository_url`,
/// unless it says `enabled=0`.
pub fn rpm_repo_has_teamcraft(text: &str, repository_url: &str) -> bool {
  let repository_url = repository_url.to_ascii_lowercase();
  let mut sections: Vec<RepoEntry> = Vec::new();
  for line in text.lines().map(str::trim) {
    if line.starts_with(['#', ';']) {
      continue;
    }
    if let Some(name) = line.strip_prefix('[').and_then(|rest| rest.strip_suffix(']')) {
      sections.push(RepoEntry::new(is_teamcraft_name(name)));
      continue;
    }
    let (Some(section), Some((key, value))) = (sections.last_mut(), line.split_once('=')) else {
      continue;
    };
    let value = value.trim().to_ascii_lowercase();
    match key.trim().to_ascii_lowercase().as_str() {
      "enabled" => section.is_enabled = value != "0",
      "baseurl" | "mirrorlist" | "metalink" => {
        section.has_url |= value
          .split([',', ' '])
          .any(|url| url.starts_with(&repository_url));
      },
      _ => {},
    }
  }
  sections.iter().any(RepoEntry::is_teamcraft)
}

/// Whether an APT source file enables TeamCraft's repository: an entry for `repository_url`, or any
/// entry in a file named `teamcraft…`. Reads one-line `.list` files and deb822 `.sources` files.
pub fn apt_source_has_teamcraft(file_name: &str, text: &str, repository_url: &str) -> bool {
  let repository_url = repository_url.to_ascii_lowercase();
  let is_named = is_teamcraft_name(file_name);
  let entries = if file_name.ends_with(".sources") {
    deb822_entries(text, &repository_url, is_named)
  } else {
    one_line_entries(text, &repository_url, is_named)
  };
  entries.iter().any(RepoEntry::is_teamcraft)
}

/// One repository of a source file, as far as finding TeamCraft's goes.
struct RepoEntry {
  is_named: bool,
  is_enabled: bool,
  has_url: bool,
}

impl RepoEntry {
  fn new(is_named: bool) -> Self {
    Self {
      is_named,
      is_enabled: true,
      has_url: false,
    }
  }

  fn is_teamcraft(&self) -> bool {
    self.is_enabled && (self.is_named || self.has_url)
  }
}

/// `deb [options] URI suite components` lines; `deb-src` and comments don't count.
fn one_line_entries(text: &str, repository_url: &str, is_named: bool) -> Vec<RepoEntry> {
  text
    .lines()
    .map(str::trim)
    .filter(|line| line.split_whitespace().next() == Some("deb"))
    .map(|line| RepoEntry {
      has_url: line
        .split_whitespace()
        .any(|word| word.to_ascii_lowercase().starts_with(repository_url)),
      ..RepoEntry::new(is_named)
    })
    .collect()
}

/// deb822 stanzas (separated by blank lines) with `Types: deb`; `Enabled: no` turns one off.
fn deb822_entries(text: &str, repository_url: &str, is_named: bool) -> Vec<RepoEntry> {
  let mut entries = Vec::new();
  let mut maybe_current: Option<(RepoEntry, bool)> = None;
  for line in text.lines().chain(std::iter::once("")) {
    let line = line.trim();
    if line.is_empty() {
      if let Some((entry, true)) = maybe_current.take() {
        entries.push(entry);
      }
      continue;
    }
    if line.starts_with('#') {
      continue;
    }
    let Some((key, value)) = line.split_once(':') else {
      continue;
    };
    let (entry, is_deb) = maybe_current.get_or_insert_with(|| (RepoEntry::new(is_named), false));
    let value = value.trim().to_ascii_lowercase();
    match key.trim().to_ascii_lowercase().as_str() {
      "types" => *is_deb = value.split_whitespace().any(|kind| kind == "deb"),
      "uris" => {
        entry.has_url |= value
          .split_whitespace()
          .any(|url| url.starts_with(repository_url))
      },
      "enabled" => entry.is_enabled = value != "no",
      _ => {},
    }
  }
  entries
}

fn is_teamcraft_name(name: &str) -> bool {
  name.trim().to_ascii_lowercase().starts_with("teamcraft")
}

/// Decides whether and how to check (docs/updates.md §4.2, §4.4). The machine switch and the
/// policy win over the user's preference; deb, rpm and Flatpak installs still check so they can
/// tell the user, on the same schedule as the installs that update themselves.
pub fn plan(inputs: PlanInputs<'_>) -> UpdatePlan {
  let policy = inputs.maybe_policy.cloned().unwrap_or_default();
  let maybe_blocker = match inputs.install {
    InstallKind::Unpackaged => Some(UpdateBlocker::Unpackaged),
    _ if inputs.is_disabled_on_machine => Some(UpdateBlocker::Machine),
    // Unknown modes from a newer policy are treated like "disabled": the stricter reading.
    _ if policy.mode != ClientUpdateMode::Allowed => Some(UpdateBlocker::Policy),
    _ if inputs.preference == UpdatePreference::Off => Some(UpdateBlocker::Preference),
    install if install.updates_itself() && !inputs.has_update_key => Some(UpdateBlocker::NotConfigured),
    _ => None,
  };
  if let Some(blocker) = maybe_blocker {
    return UpdatePlan::Off(blocker);
  }
  let delivery = match inputs.install.maybe_package_manager() {
    Some(manager) => Delivery::PackageManager(manager),
    None => Delivery::SelfUpdate {
      is_automatic: inputs.preference == UpdatePreference::Automatic,
    },
  };
  UpdatePlan::Check(CheckPlan {
    // An unknown channel falls back to stable.
    channel: match policy.channel {
      UpdateChannel::Prerelease => UpdateChannel::Prerelease,
      _ => UpdateChannel::Stable,
    },
    maybe_pinned: policy
      .maybe_pinned_version
      .as_deref()
      .and_then(|version| ClientVersion::parse(version).ok()),
    maybe_feed_url: policy.maybe_feed_url.filter(|url| is_https(url)),
    delivery,
  })
}

/// Whether a release the feed announces is offered: newer than what runs, and the pinned version if
/// one is set.
pub fn is_offered(
  current: &ClientVersion,
  remote: &ClientVersion,
  maybe_pinned: Option<&ClientVersion>,
) -> bool {
  remote > current && maybe_pinned.is_none_or(|pinned| remote == pinned)
}

/// The version this client must at least run: the higher of the server's and the policy's minimum.
/// Unreadable versions are ignored.
pub fn required_version(
  maybe_server_minimum: Option<&str>,
  maybe_policy_minimum: Option<&str>,
) -> Option<ClientVersion> {
  [maybe_server_minimum, maybe_policy_minimum]
    .into_iter()
    .flatten()
    .filter_map(|text| ClientVersion::parse(text).ok())
    .max()
}

/// The required version if `current` is older, for [`UpdateStatus::maybe_required_version`].
pub fn missing_version(current: &ClientVersion, maybe_required: Option<&ClientVersion>) -> Option<String> {
  maybe_required
    .filter(|required| current < *required)
    .map(ToString::to_string)
}

/// How long to wait before the next check. `jitter` is a random number in `[0, 1)`.
pub fn next_check_delay(is_first: bool, jitter: f64) -> Duration {
  let jitter = jitter.clamp(0.0, 1.0);
  if is_first {
    FIRST_CHECK_MIN + (FIRST_CHECK_MAX - FIRST_CHECK_MIN).mul_f64(jitter)
  } else {
    CHECK_INTERVAL - CHECK_JITTER + (CHECK_JITTER * 2).mul_f64(jitter)
  }
}

impl UpdateStatus {
  /// The banner for this status, unless the user dismissed its version ([`Self::dismissal`]).
  pub fn banner(&self) -> Option<UpdateBanner> {
    if let Dismissal::ForVersion(version) = self.dismissal()
      && self.maybe_dismissed_version.as_ref() == Some(&version)
    {
      return None;
    }
    self.undismissed_banner()
  }

  /// How the banner can be hidden: never while too old or while a self-installed update waits for
  /// its restart (#729); for good per version where the app can't install the update (#731);
  /// otherwise until the next launch.
  pub fn dismissal(&self) -> Dismissal {
    if self.maybe_required_version.is_some() {
      return Dismissal::Never;
    }
    match &self.state {
      UpdateState::Ready { .. } if self.install.updates_itself() => Dismissal::Never,
      UpdateState::Ready { version, .. } => Dismissal::ForVersion(version.clone()),
      UpdateState::Available { version, .. } if self.maybe_manual.is_some() => {
        Dismissal::ForVersion(version.clone())
      },
      UpdateState::Available { .. } => Dismissal::UntilRelaunch,
      _ => Dismissal::Never,
    }
  }

  fn undismissed_banner(&self) -> Option<UpdateBanner> {
    if let Some(required_version) = &self.maybe_required_version {
      return Some(UpdateBanner::TooOld {
        required_version: required_version.clone(),
        action: self.too_old_action(),
      });
    }
    match &self.state {
      UpdateState::Ready {
        version,
        maybe_notes_url,
      } => Some(UpdateBanner::Ready {
        version: version.clone(),
        maybe_notes_url: maybe_notes_url.clone(),
        can_restart: !self.is_in_call,
      }),
      UpdateState::Available {
        version,
        maybe_notes_url,
      } => Some(UpdateBanner::Available {
        version: version.clone(),
        maybe_notes_url: maybe_notes_url.clone(),
        action: match &self.maybe_manual {
          Some(manual) => AvailableAction::Manual(manual.clone()),
          None => AvailableAction::Download {
            can_download: !self.is_in_call,
          },
        },
      }),
      _ => None,
    }
  }

  /// Whether "Restart to update" may run now.
  pub fn can_restart(&self) -> bool {
    matches!(self.state, UpdateState::Ready { .. }) && !self.is_in_call
  }

  fn too_old_action(&self) -> TooOldAction {
    match (&self.state, &self.maybe_manual) {
      (UpdateState::Off { .. }, _) => TooOldAction::Elsewhere,
      (_, Some(manual)) => TooOldAction::Manual(manual.clone()),
      (state, None) if self.install.updates_itself() => TooOldAction::SelfUpdate(match state {
        UpdateState::Ready { .. } => SelfUpdateStep::Restart {
          can_restart: !self.is_in_call,
        },
        UpdateState::Available { .. } => SelfUpdateStep::Download {
          can_download: !self.is_in_call,
        },
        _ => SelfUpdateStep::Wait,
      }),
      _ => TooOldAction::Elsewhere,
    }
  }
}

fn is_https(url: &str) -> bool {
  url
    .get(..8)
    .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
    && url.len() > 8
}

#[cfg(test)]
mod tests {
  use super::*;

  mod versions {
    use super::*;

    #[test]
    fn candidates_come_before_their_release_and_numbers_compare_numerically() {
      let order = [
        "0.9.9",
        "1.0.0-beta.2",
        "1.0.0-rc.2",
        "1.0.0-rc.10",
        "1.0.0",
        "1.0.1",
        "1.10.0",
      ];
      let parsed: Vec<ClientVersion> = order.iter().map(|text| version(text)).collect();
      for pair in parsed.windows(2) {
        assert!(pair[0] < pair[1], "{} < {}", pair[0], pair[1]);
      }
    }

    #[test]
    fn a_leading_v_and_whitespace_are_accepted_and_garbage_is_not() {
      assert_eq!(version(" v1.2.3 "), version("1.2.3"));
      assert!(version("1.0.0-rc.1").is_prerelease());
      assert!(!version("1.0.0").is_prerelease());
      for bad in ["", "1", "1.2", "latest", "1.2.3.4", "v"] {
        assert_eq!(
          ClientVersion::parse(bad),
          Err(UpdateError::InvalidVersion(bad.to_owned()))
        );
      }
    }

    #[test]
    fn only_newer_releases_are_offered_and_a_pin_allows_exactly_one() {
      let current = version("1.0.0");
      assert!(is_offered(&current, &version("1.0.1"), None));
      assert!(!is_offered(&current, &version("1.0.0"), None));
      assert!(!is_offered(&current, &version("0.9.0"), None));
      assert!(!is_offered(&current, &version("1.0.0-rc.9"), None));

      let pinned = version("1.0.2");
      assert!(is_offered(&current, &version("1.0.2"), Some(&pinned)));
      assert!(!is_offered(&current, &version("1.0.3"), Some(&pinned)));
      assert!(!is_offered(&version("1.0.3"), &version("1.0.2"), Some(&pinned)));
    }

    #[test]
    fn a_candidate_updates_to_later_candidates_and_to_the_release() {
      let current = version("1.0.0-rc.6");
      assert!(is_offered(&current, &version("1.0.0-rc.7"), None));
      assert!(is_offered(&current, &version("1.0.0"), None));
      assert!(!is_offered(&current, &version("1.0.0-rc.5"), None));
    }
  }

  mod minimum {
    use super::*;

    #[test]
    fn the_higher_minimum_wins_and_unreadable_ones_are_ignored() {
      assert_eq!(required_version(None, None), None);
      assert_eq!(required_version(Some("1.0.0"), None), Some(version("1.0.0")));
      assert_eq!(
        required_version(Some("1.0.0"), Some("1.2.0")),
        Some(version("1.2.0"))
      );
      assert_eq!(
        required_version(Some("1.3.0"), Some("1.2.0")),
        Some(version("1.3.0"))
      );
      assert_eq!(
        required_version(Some("soon"), Some("1.2.0")),
        Some(version("1.2.0"))
      );
    }

    #[test]
    fn only_older_clients_are_asked_to_update() {
      let required = version("1.2.0");
      assert_eq!(
        missing_version(&version("1.1.9"), Some(&required)),
        Some("1.2.0".to_owned())
      );
      assert_eq!(
        missing_version(&version("1.2.0-rc.1"), Some(&required)),
        Some("1.2.0".to_owned())
      );
      assert_eq!(missing_version(&version("1.2.0"), Some(&required)), None);
      assert_eq!(missing_version(&version("2.0.0"), Some(&required)), None);
      assert_eq!(missing_version(&version("1.0.0"), None), None);
    }
  }

  mod planning {
    use super::*;

    #[test]
    fn defaults_check_stable_and_download_automatically() {
      let plan = plan(inputs(
        InstallKind::WindowsNsis,
        UpdatePreference::Automatic,
        None,
      ));
      assert_eq!(
        plan,
        UpdatePlan::Check(CheckPlan {
          channel: UpdateChannel::Stable,
          maybe_pinned: None,
          maybe_feed_url: None,
          delivery: Delivery::SelfUpdate { is_automatic: true },
        })
      );
    }

    #[test]
    fn notify_only_checks_without_downloading() {
      let UpdatePlan::Check(check) = plan(inputs(InstallKind::MacApp, UpdatePreference::NotifyOnly, None))
      else {
        panic!("expected a check");
      };
      assert_eq!(check.delivery, Delivery::SelfUpdate { is_automatic: false });
    }

    #[test]
    fn every_blocker_in_order_of_precedence() {
      let disabled = ClientUpdatePolicy {
        mode: ClientUpdateMode::Disabled,
        ..ClientUpdatePolicy::default()
      };
      let cases = [
        (
          InstallKind::Unpackaged,
          UpdatePreference::Automatic,
          None,
          true,
          true,
          UpdateBlocker::Unpackaged,
        ),
        (
          InstallKind::Flatpak,
          UpdatePreference::Automatic,
          None,
          true,
          true,
          UpdateBlocker::Machine,
        ),
        (
          InstallKind::AppImage,
          UpdatePreference::Automatic,
          Some(&disabled),
          true,
          true,
          UpdateBlocker::Machine,
        ),
        (
          InstallKind::AppImage,
          UpdatePreference::Automatic,
          Some(&disabled),
          false,
          true,
          UpdateBlocker::Policy,
        ),
        (
          InstallKind::Deb,
          UpdatePreference::Automatic,
          Some(&disabled),
          false,
          true,
          UpdateBlocker::Policy,
        ),
        (
          InstallKind::AppImage,
          UpdatePreference::Off,
          None,
          false,
          true,
          UpdateBlocker::Preference,
        ),
        (
          InstallKind::WindowsMsi,
          UpdatePreference::Automatic,
          None,
          false,
          false,
          UpdateBlocker::NotConfigured,
        ),
      ];
      for (install, preference, maybe_policy, is_disabled_on_machine, has_update_key, blocker) in cases {
        let actual = plan(PlanInputs {
          install,
          preference,
          maybe_policy,
          is_disabled_on_machine,
          has_update_key,
        });
        assert_eq!(actual, UpdatePlan::Off(blocker), "{install:?}");
      }
    }

    #[test]
    fn an_unknown_policy_mode_counts_as_disabled_and_an_unknown_channel_as_stable() {
      let unknown_mode = ClientUpdatePolicy {
        mode: ClientUpdateMode::Unknown("ask_it".to_owned()),
        ..ClientUpdatePolicy::default()
      };
      assert_eq!(
        plan(inputs(
          InstallKind::AppImage,
          UpdatePreference::Automatic,
          Some(&unknown_mode)
        )),
        UpdatePlan::Off(UpdateBlocker::Policy)
      );
      let unknown_channel = ClientUpdatePolicy {
        channel: UpdateChannel::Unknown("nightly".to_owned()),
        ..ClientUpdatePolicy::default()
      };
      let UpdatePlan::Check(check) = plan(inputs(
        InstallKind::AppImage,
        UpdatePreference::Automatic,
        Some(&unknown_channel),
      )) else {
        panic!("expected a check");
      };
      assert_eq!(check.channel, UpdateChannel::Stable);
    }

    #[test]
    fn deb_rpm_and_flatpak_check_without_an_update_key_and_hand_over_to_the_package_manager() {
      for (install, manager) in [
        (InstallKind::Deb, PackageManager::Apt),
        (InstallKind::Rpm, PackageManager::Dnf),
        (InstallKind::Flatpak, PackageManager::Flatpak),
      ] {
        let actual = plan(PlanInputs {
          has_update_key: false,
          ..inputs(install, UpdatePreference::Automatic, None)
        });
        let UpdatePlan::Check(check) = actual else {
          panic!("expected a check for {install:?}");
        };
        assert_eq!(check.delivery, Delivery::PackageManager(manager));
      }
    }

    #[test]
    fn policy_channel_pin_and_https_feed_are_applied() {
      let policy = ClientUpdatePolicy {
        channel: UpdateChannel::Prerelease,
        maybe_pinned_version: Some("v1.4.0".to_owned()),
        maybe_feed_url: Some("https://updates.example.com/latest.json".to_owned()),
        ..ClientUpdatePolicy::default()
      };
      let UpdatePlan::Check(check) = plan(inputs(
        InstallKind::WindowsNsis,
        UpdatePreference::Automatic,
        Some(&policy),
      )) else {
        panic!("expected a check");
      };
      assert_eq!(check.channel, UpdateChannel::Prerelease);
      assert_eq!(check.maybe_pinned, Some(version("1.4.0")));
      assert_eq!(
        check.maybe_feed_url.as_deref(),
        Some("https://updates.example.com/latest.json")
      );
    }

    #[test]
    fn plain_http_feeds_and_unreadable_pins_are_ignored() {
      let policy = ClientUpdatePolicy {
        maybe_pinned_version: Some("next".to_owned()),
        maybe_feed_url: Some("http://updates.example.com/latest.json".to_owned()),
        ..ClientUpdatePolicy::default()
      };
      let UpdatePlan::Check(check) = plan(inputs(
        InstallKind::AppImage,
        UpdatePreference::Automatic,
        Some(&policy),
      )) else {
        panic!("expected a check");
      };
      assert_eq!(check.maybe_pinned, None);
      assert_eq!(check.maybe_feed_url, None);
      assert!(!is_https("https://"));
      assert!(is_https("HTTPS://mirror.example.com/latest.json"));
    }

    #[test]
    fn package_manager_commands_name_the_package() {
      assert_eq!(
        PackageManager::Apt.command(),
        "sudo apt update && sudo apt install --only-upgrade team-craft"
      );
      assert_eq!(
        PackageManager::Dnf.command(),
        "sudo dnf upgrade --refresh team-craft"
      );
      assert_eq!(
        PackageManager::Flatpak.command(),
        "flatpak update de.zeitgleich.teamcraft"
      );
      assert!(PackageManager::Dnf.is_package_kit_managed());
      assert!(!PackageManager::Flatpak.is_package_kit_managed());
    }

    #[test]
    fn package_files_are_named_like_the_release_assets() {
      let release = version("1.0.8");
      assert_eq!(
        linux_package_file(PackageManager::Apt, &release, "x86_64").as_deref(),
        Some("TeamCraft_1.0.8_amd64.deb")
      );
      assert_eq!(
        linux_package_file(PackageManager::Apt, &release, "aarch64").as_deref(),
        Some("TeamCraft_1.0.8_arm64.deb")
      );
      assert_eq!(
        linux_package_file(PackageManager::Dnf, &release, "x86_64").as_deref(),
        Some("TeamCraft-1.0.8-1.x86_64.rpm")
      );
      assert_eq!(
        linux_package_file(PackageManager::Dnf, &release, "aarch64").as_deref(),
        Some("TeamCraft-1.0.8-1.aarch64.rpm")
      );
      assert_eq!(linux_package_file(PackageManager::Dnf, &release, "riscv64"), None);
      assert_eq!(
        linux_package_file(PackageManager::Flatpak, &release, "x86_64"),
        None
      );
      assert_eq!(
        linux_package_file(PackageManager::Apt, &version("1.1.0-rc.1"), "x86_64"),
        None
      );
    }
  }

  mod repositories {
    use super::*;

    const RPM_URL: &str = "https://example-org.github.io/TeamCraft/rpm/";
    const APT_URL: &str = "https://example-org.github.io/TeamCraft/apt";

    #[test]
    fn the_documented_rpm_repository_counts_by_name_or_url() {
      let documented = "[teamcraft]\nname=TeamCraft\nbaseurl=https://example-org.github.io/TeamCraft/rpm/stable\nenabled=1\n";
      assert!(rpm_repo_has_teamcraft(documented, RPM_URL));
      let renamed = "[company-chat]\nbaseurl = HTTPS://Example-Org.github.io/TeamCraft/rpm/prerelease\n";
      assert!(rpm_repo_has_teamcraft(renamed, RPM_URL));
      let mirror = "[teamcraft-prerelease]\nbaseurl=https://mirror.example.com/teamcraft/\n";
      assert!(rpm_repo_has_teamcraft(mirror, RPM_URL));
    }

    #[test]
    fn disabled_commented_and_other_rpm_repositories_do_not_count() {
      let disabled = "[teamcraft]\nbaseurl=https://example-org.github.io/TeamCraft/rpm/stable\nenabled=0\n";
      assert!(!rpm_repo_has_teamcraft(disabled, RPM_URL));
      let commented = "#[teamcraft]\n#baseurl=https://example-org.github.io/TeamCraft/rpm/stable\n";
      assert!(!rpm_repo_has_teamcraft(commented, RPM_URL));
      let fedora = "[fedora]\nmetalink=https://mirrors.example.org/metalink?repo=fedora-43\nenabled=1\n";
      assert!(!rpm_repo_has_teamcraft(fedora, RPM_URL));
      // Only the TeamCraft section of a shared file counts, and only while it is enabled.
      let shared = format!("{fedora}\n[teamcraft]\nbaseurl={RPM_URL}stable\nenabled=0\n");
      assert!(!rpm_repo_has_teamcraft(&shared, RPM_URL));
      assert!(!rpm_repo_has_teamcraft("", RPM_URL));
      assert!(!rpm_repo_has_teamcraft(
        "baseurl=https://example-org.github.io/TeamCraft/rpm/\n",
        RPM_URL
      ));
    }

    #[test]
    fn deb822_sources_count_unless_disabled() {
      let documented =
        "Types: deb\nURIs: https://example-org.github.io/TeamCraft/apt\nSuites: stable\nComponents: main\n";
      assert!(apt_source_has_teamcraft("teamcraft.sources", documented, APT_URL));
      assert!(apt_source_has_teamcraft("chat.sources", documented, APT_URL));
      let disabled = format!("{documented}Enabled: no\n");
      assert!(!apt_source_has_teamcraft("teamcraft.sources", &disabled, APT_URL));
      let source_only = "Types: deb-src\nURIs: https://example-org.github.io/TeamCraft/apt\n";
      assert!(!apt_source_has_teamcraft(
        "teamcraft.sources",
        source_only,
        APT_URL
      ));
      let other = "Types: deb\nURIs: http://archive.example.org/ubuntu\nSuites: noble\n\n";
      assert!(!apt_source_has_teamcraft("ubuntu.sources", other, APT_URL));
      assert!(apt_source_has_teamcraft(
        "ubuntu.sources",
        &format!("{other}{documented}"),
        APT_URL
      ));
    }

    #[test]
    fn one_line_sources_count_by_url_or_file_name() {
      let line = "deb [signed-by=/usr/share/keyrings/teamcraft-archive-keyring.gpg] https://example-org.github.io/TeamCraft/apt stable main\n";
      assert!(apt_source_has_teamcraft("sources.list", line, APT_URL));
      let mirror = "deb https://mirror.example.com/apt stable main\n";
      assert!(apt_source_has_teamcraft("teamcraft.list", mirror, APT_URL));
      assert!(!apt_source_has_teamcraft("other.list", mirror, APT_URL));
      assert!(!apt_source_has_teamcraft(
        "teamcraft.list",
        &format!("# {line}"),
        APT_URL
      ));
      assert!(!apt_source_has_teamcraft("teamcraft.list", "", APT_URL));
    }
  }

  mod scheduling {
    use super::*;

    #[test]
    fn the_first_check_comes_within_minutes_and_later_ones_about_daily() {
      assert_eq!(next_check_delay(true, 0.0), Duration::from_secs(60));
      assert_eq!(next_check_delay(true, 1.0), Duration::from_secs(600));
      assert_eq!(next_check_delay(false, 0.0), Duration::from_secs(22 * 3600));
      assert_eq!(next_check_delay(false, 0.5), Duration::from_secs(24 * 3600));
      assert_eq!(next_check_delay(false, 1.0), Duration::from_secs(26 * 3600));
      // Out-of-range jitter is clamped, never negative or unbounded.
      assert_eq!(next_check_delay(false, -3.0), Duration::from_secs(22 * 3600));
      assert_eq!(next_check_delay(true, f64::INFINITY), Duration::from_secs(600));
    }
  }

  mod banners {
    use super::*;

    #[test]
    fn nothing_is_shown_while_idle_checking_or_downloading() {
      for state in [
        UpdateState::Idle {
          maybe_checked_at: None,
        },
        UpdateState::Checking,
        UpdateState::Downloading {
          version: "1.0.1".into(),
          maybe_percent: Some(40),
        },
        UpdateState::Off {
          reason: UpdateBlocker::Preference,
        },
        UpdateState::Failed {
          message: "offline".into(),
        },
      ] {
        assert_eq!(status(state).banner(), None);
      }
    }

    #[test]
    fn a_ready_update_offers_a_restart_except_during_a_call() {
      let ready = UpdateState::Ready {
        version: "1.0.1".into(),
        maybe_notes_url: Some("https://example.com/notes".into()),
      };
      let mut current = status(ready);
      assert_eq!(
        current.banner(),
        Some(UpdateBanner::Ready {
          version: "1.0.1".into(),
          maybe_notes_url: Some("https://example.com/notes".into()),
          can_restart: true,
        })
      );
      assert!(current.can_restart());
      current.is_in_call = true;
      assert!(matches!(
        current.banner(),
        Some(UpdateBanner::Ready {
          can_restart: false,
          ..
        })
      ));
      assert!(!current.can_restart());
    }

    #[test]
    fn an_available_update_offers_a_download_or_the_package_manager() {
      let available = UpdateState::Available {
        version: "1.0.1".into(),
        maybe_notes_url: None,
      };
      let mut current = status(available);
      assert!(matches!(
        current.banner(),
        Some(UpdateBanner::Available {
          action: AvailableAction::Download { can_download: true },
          ..
        })
      ));
      current.is_in_call = true;
      assert!(matches!(
        current.banner(),
        Some(UpdateBanner::Available {
          action: AvailableAction::Download { can_download: false },
          ..
        })
      ));
      current.install = InstallKind::Deb;
      current.maybe_manual = Some(ManualUpdate::PackageManager(apt_hint()));
      assert_eq!(
        current.banner(),
        Some(UpdateBanner::Available {
          version: "1.0.1".into(),
          maybe_notes_url: None,
          action: AvailableAction::Manual(ManualUpdate::PackageManager(apt_hint())),
        })
      );
    }

    #[test]
    fn hints_the_app_cannot_act_on_are_dismissed_per_version() {
      let mut current = status(UpdateState::Available {
        version: "1.0.8".into(),
        maybe_notes_url: None,
      });
      current.install = InstallKind::Rpm;
      current.maybe_manual = Some(file_download());
      assert_eq!(current.dismissal(), Dismissal::ForVersion("1.0.8".into()));
      current.maybe_dismissed_version = Some("1.0.7".into());
      assert!(
        current.banner().is_some(),
        "an older dismissal doesn't hide a new version"
      );
      current.maybe_dismissed_version = Some("1.0.8".into());
      assert_eq!(current.banner(), None);
      // The next version shows again.
      current.state = UpdateState::Available {
        version: "1.0.9".into(),
        maybe_notes_url: None,
      };
      assert!(current.banner().is_some());
      // After PackageKit installed it, the restart hint is dismissable too.
      current.state = UpdateState::Ready {
        version: "1.0.9".into(),
        maybe_notes_url: None,
      };
      assert_eq!(current.dismissal(), Dismissal::ForVersion("1.0.9".into()));
      // A version that is too old can't be hidden, whatever was dismissed.
      current.maybe_dismissed_version = Some("1.0.9".into());
      current.maybe_required_version = Some("1.0.9".into());
      assert_eq!(current.dismissal(), Dismissal::Never);
      assert!(matches!(
        current.banner(),
        Some(UpdateBanner::TooOld {
          action: TooOldAction::Manual(_),
          ..
        })
      ));
    }

    #[test]
    fn a_self_installed_update_waits_for_its_restart_and_a_found_one_for_the_next_launch() {
      let mut current = status(UpdateState::Ready {
        version: "1.0.8".into(),
        maybe_notes_url: None,
      });
      assert_eq!(current.dismissal(), Dismissal::Never);
      current.maybe_dismissed_version = Some("1.0.8".into());
      assert!(current.banner().is_some());
      current.state = UpdateState::Available {
        version: "1.0.8".into(),
        maybe_notes_url: None,
      };
      assert_eq!(current.dismissal(), Dismissal::UntilRelaunch);
      assert!(current.banner().is_some());
      // An AppImage in a read-only place: the app can't install it, so it is a hint.
      current.maybe_manual = Some(ManualUpdate::Download {
        url: "https://example.com/TeamCraft_1.0.8_amd64.AppImage".into(),
        maybe_repository_setup_url: None,
      });
      assert_eq!(current.dismissal(), Dismissal::ForVersion("1.0.8".into()));
      assert_eq!(current.banner(), None);
    }

    #[test]
    fn too_old_wins_over_everything_and_says_who_can_update() {
      let mut current = status(UpdateState::Ready {
        version: "1.3.0".into(),
        maybe_notes_url: None,
      });
      current.maybe_required_version = Some("1.2.0".into());
      assert_eq!(
        current.banner(),
        Some(UpdateBanner::TooOld {
          required_version: "1.2.0".into(),
          action: TooOldAction::SelfUpdate(SelfUpdateStep::Restart { can_restart: true }),
        })
      );
      current.is_in_call = true;
      current.state = UpdateState::Available {
        version: "1.3.0".into(),
        maybe_notes_url: None,
      };
      assert!(matches!(
        current.banner(),
        Some(UpdateBanner::TooOld {
          action: TooOldAction::SelfUpdate(SelfUpdateStep::Download { can_download: false }),
          ..
        })
      ));
      current.is_in_call = false;
      current.state = UpdateState::Checking;
      assert!(matches!(
        current.banner(),
        Some(UpdateBanner::TooOld {
          action: TooOldAction::SelfUpdate(SelfUpdateStep::Wait),
          ..
        })
      ));

      current.state = UpdateState::Off {
        reason: UpdateBlocker::Policy,
      };
      assert!(matches!(
        current.banner(),
        Some(UpdateBanner::TooOld {
          action: TooOldAction::Elsewhere,
          ..
        })
      ));

      current.state = UpdateState::Idle {
        maybe_checked_at: None,
      };
      current.install = InstallKind::Rpm;
      current.maybe_manual = Some(ManualUpdate::PackageManager(apt_hint()));
      assert!(matches!(
        current.banner(),
        Some(UpdateBanner::TooOld {
          action: TooOldAction::Manual(_),
          ..
        })
      ));

      current.install = InstallKind::Unpackaged;
      current.maybe_manual = None;
      assert!(matches!(
        current.banner(),
        Some(UpdateBanner::TooOld {
          action: TooOldAction::Elsewhere,
          ..
        })
      ));
    }

    #[test]
    fn status_wire_format_is_stable() {
      let mut current = status(UpdateState::Downloading {
        version: "1.0.1".into(),
        maybe_percent: Some(12),
      });
      current.maybe_manual = Some(ManualUpdate::PackageManager(apt_hint()));
      current.maybe_dismissed_version = Some("1.0.0".into());
      let json = serde_json::to_value(&current).unwrap_or_default();
      assert_eq!(
        json,
        serde_json::json!({
          "current_version": "1.0.0",
          "install": "app_image",
          "preference": "automatic",
          "state": {"kind": "downloading", "version": "1.0.1", "percent": 12},
          "is_in_call": false,
          "manual": {
            "kind": "package_manager",
            "manager": "apt",
            "command": "sudo apt update && sudo apt install --only-upgrade team-craft",
            "has_package_kit": true,
          },
          "dismissed_version": "1.0.0",
        })
      );
      let parsed: UpdateStatus =
        serde_json::from_value(json).unwrap_or_else(|_| status(UpdateState::Checking));
      assert_eq!(parsed, current);

      current.maybe_manual = Some(file_download());
      let json = serde_json::to_value(&current).unwrap_or_default();
      assert_eq!(
        json["manual"],
        serde_json::json!({
          "kind": "download",
          "url": "https://example.com/TeamCraft-1.0.8-1.x86_64.rpm",
          "repository_setup_url": "https://example.com/",
        })
      );
    }
  }

  fn version(text: &str) -> ClientVersion {
    ClientVersion::parse(text).unwrap_or_else(|error| panic!("{error}"))
  }

  fn inputs(
    install: InstallKind,
    preference: UpdatePreference,
    maybe_policy: Option<&ClientUpdatePolicy>,
  ) -> PlanInputs<'_> {
    PlanInputs {
      install,
      preference,
      maybe_policy,
      is_disabled_on_machine: false,
      has_update_key: true,
    }
  }

  fn status(state: UpdateState) -> UpdateStatus {
    UpdateStatus {
      current_version: "1.0.0".into(),
      install: InstallKind::AppImage,
      preference: UpdatePreference::Automatic,
      state,
      maybe_required_version: None,
      is_in_call: false,
      maybe_manual: None,
      maybe_dismissed_version: None,
    }
  }

  fn file_download() -> ManualUpdate {
    ManualUpdate::Download {
      url: "https://example.com/TeamCraft-1.0.8-1.x86_64.rpm".into(),
      maybe_repository_setup_url: Some("https://example.com/".into()),
    }
  }

  fn apt_hint() -> PackageManagerHint {
    PackageManagerHint {
      manager: PackageManager::Apt,
      command: PackageManager::Apt.command(),
      has_package_kit: true,
    }
  }
}
