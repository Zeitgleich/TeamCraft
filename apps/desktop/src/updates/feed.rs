//! Where updates come from and what this build is (docs/updates.md §2, §3).

use std::path::Path;

use tauri::utils::config::BundleType;
use teamcraft_client_api::updates::{
  CheckPlan, ClientVersion, InstallKind, PackageManager, PackageSource, apt_source_has_teamcraft,
  linux_package_file, rpm_repo_has_teamcraft,
};
use teamcraft_proto::UpdateChannel;

/// Where dnf and zypper keep their repository files.
const RPM_REPO_DIRS: [&str; 2] = ["/etc/yum.repos.d", "/etc/zypp/repos.d"];
const APT_SOURCES_FILE: &str = "/etc/apt/sources.list";
const APT_SOURCES_DIR: &str = "/etc/apt/sources.list.d";

/// The version of this build: the release tag's version (`1.0.0-rc.2`), which the release workflow
/// passes in, or the crate version for local builds.
pub const BUILD_VERSION: &str = match option_env!("TEAMCRAFT_RELEASE_VERSION") {
  Some(version) => version,
  None => env!("CARGO_PKG_VERSION"),
};

/// The GitHub repository whose releases this build follows (`owner/name`); a fork's release
/// workflow passes its own.
const RELEASE_REPOSITORY: &str = match option_env!("TEAMCRAFT_RELEASE_REPOSITORY") {
  Some(repository) => repository,
  None => "Zeitgleich/TeamCraft",
};

/// What the manifest says; the rest of it is the updater plugin's business.
#[derive(Debug, serde::Deserialize)]
pub struct ManifestSummary {
  pub version: String,
  #[serde(default)]
  pub notes: Option<String>,
}

/// The version this build reports. An unreadable injected version falls back to the crate version,
/// which Cargo requires to be semantic.
pub fn build_version() -> Option<ClientVersion> {
  ClientVersion::parse(BUILD_VERSION)
    .or_else(|_| ClientVersion::parse(env!("CARGO_PKG_VERSION")))
    .ok()
}

/// How this copy was installed.
pub fn install_kind() -> InstallKind {
  if cfg!(debug_assertions) {
    return InstallKind::Unpackaged;
  }
  if std::env::var_os("FLATPAK_ID").is_some() || Path::new("/.flatpak-info").exists() {
    return InstallKind::Flatpak;
  }
  match tauri::utils::platform::bundle_type() {
    Some(BundleType::Nsis) => InstallKind::WindowsNsis,
    Some(BundleType::Msi) => InstallKind::WindowsMsi,
    Some(BundleType::AppImage) => InstallKind::AppImage,
    Some(BundleType::Deb) => InstallKind::Deb,
    Some(BundleType::Rpm) => InstallKind::Rpm,
    // macOS reports an app bundle for any build; only a real one can be replaced.
    Some(BundleType::App | BundleType::Dmg) if runs_from_app_bundle() => InstallKind::MacApp,
    _ => InstallKind::Unpackaged,
  }
}

/// The manifest to read for a plan (docs/updates.md §3.2).
pub fn manifest_url(plan: &CheckPlan) -> String {
  manifest_url_for(plan, RELEASE_REPOSITORY)
}

/// Whether a deb or rpm install gets its updates from TeamCraft's repository (docs/updates.md §5),
/// read from the package manager's source files. Unreadable files count as missing.
pub fn package_source(manager: PackageManager) -> PackageSource {
  let pages = pages_url(RELEASE_REPOSITORY);
  let has_repository = match manager {
    PackageManager::Dnf => RPM_REPO_DIRS
      .iter()
      .flat_map(|dir| source_files(Path::new(dir)))
      .filter(|(name, _)| name.ends_with(".repo"))
      .any(|(_, text)| rpm_repo_has_teamcraft(&text, &format!("{pages}rpm/"))),
    PackageManager::Apt => std::fs::read_to_string(APT_SOURCES_FILE)
      .ok()
      .map(|text| ("sources.list".to_owned(), text))
      .into_iter()
      .chain(source_files(Path::new(APT_SOURCES_DIR)))
      .filter(|(name, _)| name.ends_with(".list") || name.ends_with(".sources"))
      .any(|(name, text)| apt_source_has_teamcraft(&name, &text, &format!("{pages}apt"))),
    // Flatpak installs from a remote; the hint is its command either way.
    PackageManager::Flatpak => true,
  };
  if has_repository {
    PackageSource::Repository
  } else {
    PackageSource::File
  }
}

/// The page that explains how to add TeamCraft's repositories (the package site).
pub fn repository_setup_url() -> String {
  pages_url(RELEASE_REPOSITORY)
}

/// Where the new deb or rpm of `version` can be downloaded: the release asset for this machine,
/// or the release page where no asset name can be told (candidates, other architectures).
pub fn package_download_url(manager: PackageManager, version: &ClientVersion) -> String {
  package_download_url_for(manager, version, std::env::consts::ARCH, RELEASE_REPOSITORY)
}

fn package_download_url_for(
  manager: PackageManager,
  version: &ClientVersion,
  arch: &str,
  repository: &str,
) -> String {
  match linux_package_file(manager, version, arch) {
    Some(file) => format!("https://github.com/{repository}/releases/download/v{version}/{file}"),
    None => format!("https://github.com/{repository}/releases/tag/v{version}"),
  }
}

/// The releases page, for a hint before the feed named a version.
pub fn releases_url() -> String {
  format!("https://github.com/{RELEASE_REPOSITORY}/releases/latest")
}

/// The files of a directory with their names; a missing directory or an unreadable file is skipped.
fn source_files(dir: &Path) -> impl Iterator<Item = (String, String)> {
  std::fs::read_dir(dir)
    .into_iter()
    .flatten()
    .filter_map(Result::ok)
    .filter_map(|entry| {
      let name = entry.file_name().to_string_lossy().into_owned();
      std::fs::read_to_string(entry.path())
        .ok()
        .map(|text| (name, text))
    })
}

/// The repository's GitHub Pages site (`https://<owner>.github.io/<name>/`), with the package
/// repositories and the prerelease feed.
fn pages_url(repository: &str) -> String {
  let (owner, name) = repository.split_once('/').unwrap_or((repository, ""));
  format!("https://{}.github.io/{name}/", owner.to_ascii_lowercase())
}

fn manifest_url_for(plan: &CheckPlan, repository: &str) -> String {
  if let Some(feed) = &plan.maybe_feed_url {
    return feed.clone();
  }
  if let Some(pinned) = &plan.maybe_pinned {
    return format!("https://github.com/{repository}/releases/download/v{pinned}/latest.json");
  }
  match plan.channel {
    UpdateChannel::Prerelease => format!("{}updates/prerelease/latest.json", pages_url(repository)),
    _ => format!("https://github.com/{repository}/releases/latest/download/latest.json"),
  }
}

fn runs_from_app_bundle() -> bool {
  std::env::current_exe()
    .ok()
    .is_some_and(|exe| exe.to_string_lossy().contains(".app/Contents/MacOS/"))
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::updates::Delivery;

  use super::*;

  const REPOSITORY: &str = "Example-Org/TeamCraft";

  #[test]
  fn stable_uses_the_latest_release_and_prerelease_the_pages_site() {
    assert_eq!(
      manifest_url_for(&plan(UpdateChannel::Stable), REPOSITORY),
      "https://github.com/Example-Org/TeamCraft/releases/latest/download/latest.json"
    );
    assert_eq!(
      manifest_url_for(&plan(UpdateChannel::Prerelease), REPOSITORY),
      "https://example-org.github.io/TeamCraft/updates/prerelease/latest.json"
    );
  }

  #[test]
  fn a_pin_reads_that_release_and_a_feed_url_wins_over_everything() {
    let mut pinned = plan(UpdateChannel::Prerelease);
    pinned.maybe_pinned = ClientVersion::parse("1.2.0-rc.1").ok();
    assert_eq!(
      manifest_url_for(&pinned, REPOSITORY),
      "https://github.com/Example-Org/TeamCraft/releases/download/v1.2.0-rc.1/latest.json"
    );
    pinned.maybe_feed_url = Some("https://mirror.example.com/teamcraft/latest.json".to_owned());
    assert_eq!(
      manifest_url_for(&pinned, REPOSITORY),
      "https://mirror.example.com/teamcraft/latest.json"
    );
  }

  #[test]
  fn package_downloads_link_the_asset_or_the_release_page() {
    let release = ClientVersion::parse("1.0.8").unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
      package_download_url_for(PackageManager::Dnf, &release, "x86_64", REPOSITORY),
      "https://github.com/Example-Org/TeamCraft/releases/download/v1.0.8/TeamCraft-1.0.8-1.x86_64.rpm"
    );
    assert_eq!(
      package_download_url_for(PackageManager::Apt, &release, "aarch64", REPOSITORY),
      "https://github.com/Example-Org/TeamCraft/releases/download/v1.0.8/TeamCraft_1.0.8_arm64.deb"
    );
    let candidate = ClientVersion::parse("1.1.0-rc.1").unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
      package_download_url_for(PackageManager::Apt, &candidate, "x86_64", REPOSITORY),
      "https://github.com/Example-Org/TeamCraft/releases/tag/v1.1.0-rc.1"
    );
    assert_eq!(pages_url(REPOSITORY), "https://example-org.github.io/TeamCraft/");
  }

  #[test]
  fn the_build_version_is_semantic() {
    assert!(build_version().is_some());
  }

  #[test]
  fn development_builds_never_update_themselves() {
    // Tests are debug builds.
    assert_eq!(install_kind(), InstallKind::Unpackaged);
  }

  fn plan(channel: UpdateChannel) -> CheckPlan {
    CheckPlan {
      channel,
      maybe_pinned: None,
      maybe_feed_url: None,
      delivery: Delivery::SelfUpdate { is_automatic: true },
    }
  }
}
