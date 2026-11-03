//! Updating the deb/rpm package through PackageKit (docs/updates.md §4.5): the same D-Bus API the
//! software centers use. The package manager checks the repository signature (`ONLY_TRUSTED`) and
//! polkit asks the user; the app never runs a package manager or touches its files itself.

use std::collections::HashMap;
use std::time::Duration;

use futures_util::StreamExt;
use teamcraft_client_api::updates::{InstallPhase, LINUX_PACKAGE_NAME};
use zbus::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

const SERVICE: &str = "org.freedesktop.PackageKit";
const PATH: &str = "/org/freedesktop/PackageKit";
const TRANSACTION: &str = "org.freedesktop.PackageKit.Transaction";
/// Starting the service on first use can take a moment.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// A whole transaction, including the user typing a password and the download.
const TRANSACTION_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// `PK_FILTER_ENUM_NONE` as a bitfield.
const FILTER_NONE: u64 = 1 << 1;
/// `PK_TRANSACTION_FLAG_ENUM_ONLY_TRUSTED` as a bitfield: only packages from signed repositories.
const ONLY_TRUSTED: u64 = 1 << 1;
/// `PK_EXIT_ENUM_SUCCESS`.
const EXIT_SUCCESS: u32 = 1;
/// `PK_STATUS_ENUM_WAITING_FOR_AUTH`: polkit asks the user.
const STATUS_WAITING_FOR_AUTH: u32 = 28;

#[derive(Debug, thiserror::Error)]
pub enum PackageKitError {
  #[error("PackageKit is not available")]
  Unavailable,
  #[error("no update of {LINUX_PACKAGE_NAME} is available from the configured repositories")]
  NoUpdate,
  #[error("PackageKit did not finish in time")]
  TimedOut,
  #[error("PackageKit failed: {0}")]
  Failed(String),
  #[error(transparent)]
  Bus(#[from] zbus::Error),
}

/// Whether PackageKit answers on the system bus.
pub async fn is_available() -> bool {
  let Ok(Ok(connection)) = tokio::time::timeout(PROBE_TIMEOUT, Connection::system()).await else {
    return false;
  };
  tokio::time::timeout(
    PROBE_TIMEOUT,
    connection.call_method(
      Some(SERVICE),
      PATH,
      Some("org.freedesktop.DBus.Properties"),
      "Get",
      &(SERVICE, "VersionMajor"),
    ),
  )
  .await
  .is_ok_and(|reply| reply.is_ok())
}

/// Refreshes the package lists and installs the newest TeamCraft package; returns its version.
/// `on_phase` hears when polkit asks the user and when the work goes on (#1044).
pub async fn update_teamcraft(on_phase: impl Fn(InstallPhase)) -> Result<String, PackageKitError> {
  let connection = Connection::system()
    .await
    .map_err(|_| PackageKitError::Unavailable)?;
  run(&connection, "RefreshCache", &(false,), &on_phase).await?;
  let updates = run(&connection, "GetUpdates", &(FILTER_NONE,), &on_phase).await?;
  let package_id = updates
    .into_iter()
    .find(|id| package_name(id) == Some(LINUX_PACKAGE_NAME))
    .ok_or(PackageKitError::NoUpdate)?;
  run(
    &connection,
    "UpdatePackages",
    &(ONLY_TRUSTED, vec![package_id.as_str()]),
    &on_phase,
  )
  .await?;
  Ok(package_version(&package_id).unwrap_or_default().to_owned())
}

/// Runs one method in a new transaction and returns the IDs of the packages it reported.
async fn run<B>(
  connection: &Connection,
  method: &str,
  body: &B,
  on_phase: &impl Fn(InstallPhase),
) -> Result<Vec<String>, PackageKitError>
where
  B: serde::Serialize + zbus::zvariant::DynamicType,
{
  let reply = connection
    .call_method(Some(SERVICE), PATH, Some(SERVICE), "CreateTransaction", &())
    .await
    .map_err(|_| PackageKitError::Unavailable)?;
  let transaction: OwnedObjectPath = reply.body().deserialize()?;
  // Subscribed before the call, so no signal is missed: the transaction's own signals and its
  // property changes (`Status`).
  let rule = zbus::MatchRule::builder()
    .msg_type(zbus::message::Type::Signal)
    .sender(SERVICE)?
    .path(transaction.clone())?
    .build();
  let mut signals = zbus::MessageStream::for_match_rule(rule, connection, None).await?;
  // Lets polkit ask the user for a password instead of refusing.
  connection
    .call_method(
      Some(SERVICE),
      &transaction,
      Some(TRANSACTION),
      "SetHints",
      &(vec!["interactive=true"],),
    )
    .await?;
  connection
    .call_method(Some(SERVICE), &transaction, Some(TRANSACTION), method, body)
    .await?;

  let collect = async {
    let mut packages = Vec::new();
    let mut maybe_error = None;
    let mut maybe_phase = None;
    while let Some(Ok(message)) = signals.next().await {
      let header = message.header();
      match header.member().map(|member| member.as_str()) {
        Some("PropertiesChanged") => {
          if let Some(phase) = changed_phase(&message)
            && maybe_phase != Some(phase)
          {
            maybe_phase = Some(phase);
            on_phase(phase);
          }
        },
        Some("Package") => {
          if let Ok((_info, id, _summary)) = message.body().deserialize::<(u32, String, String)>() {
            packages.push(id);
          }
        },
        Some("ErrorCode") => {
          if let Ok((_code, details)) = message.body().deserialize::<(u32, String)>() {
            maybe_error = Some(details);
          }
        },
        Some("Finished") => {
          let (exit, _runtime) = message.body().deserialize::<(u32, u32)>()?;
          return if exit == EXIT_SUCCESS {
            Ok(packages)
          } else {
            Err(PackageKitError::Failed(
              maybe_error.unwrap_or_else(|| format!("{method} ended with exit code {exit}")),
            ))
          };
        },
        _ => {},
      }
    }
    Err(PackageKitError::Failed(format!(
      "{method} ended without a result"
    )))
  };
  tokio::time::timeout(TRANSACTION_TIMEOUT, collect)
    .await
    .map_err(|_| PackageKitError::TimedOut)?
}

/// The phase a transaction's `Status` change means, if it changed.
fn changed_phase(message: &zbus::Message) -> Option<InstallPhase> {
  let (_interface, changed, _invalidated) = message
    .body()
    .deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>()
    .ok()?;
  let status = u32::try_from(changed.get("Status")?).ok()?;
  Some(phase_of(status))
}

fn phase_of(status: u32) -> InstallPhase {
  if status == STATUS_WAITING_FOR_AUTH {
    InstallPhase::WaitingForAuthorization
  } else {
    InstallPhase::Running
  }
}

/// Package IDs are `name;version;arch;data`.
fn package_name(id: &str) -> Option<&str> {
  id.split(';').next()
}

fn package_version(id: &str) -> Option<&str> {
  id.split(';').nth(1)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn package_ids_are_split_into_name_and_version() {
    let id = "team-craft;1.0.1-1;x86_64;teamcraft-stable";
    assert_eq!(package_name(id), Some("team-craft"));
    assert_eq!(package_version(id), Some("1.0.1-1"));
    assert_eq!(package_name(""), Some(""));
    assert_eq!(package_version("broken"), None);
  }

  #[test]
  fn only_the_wait_for_polkit_counts_as_waiting_for_authorization() {
    assert_eq!(phase_of(28), InstallPhase::WaitingForAuthorization);
    // Refreshing the cache, downloading, installing.
    for status in [7, 8, 10] {
      assert_eq!(phase_of(status), InstallPhase::Running);
    }
  }
}
