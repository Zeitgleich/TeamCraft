//! Where updates come from and what this build is (docs/updates.md §2, §3).

use std::path::Path;

use tauri::utils::config::BundleType;
use teamcraft_client_api::updates::{CheckPlan, ClientVersion, InstallKind};
use teamcraft_proto::UpdateChannel;

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

fn manifest_url_for(plan: &CheckPlan, repository: &str) -> String {
  if let Some(feed) = &plan.maybe_feed_url {
    return feed.clone();
  }
  if let Some(pinned) = &plan.maybe_pinned {
    return format!("https://github.com/{repository}/releases/download/v{pinned}/latest.json");
  }
  match plan.channel {
    UpdateChannel::Prerelease => {
      let (owner, name) = repository.split_once('/').unwrap_or((repository, ""));
      format!(
        "https://{}.github.io/{name}/updates/prerelease/latest.json",
        owner.to_ascii_lowercase()
      )
    },
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
