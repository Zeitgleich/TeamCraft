//! The managed server from the operating system's policy store (#220): the Windows registry key
//! `HKLM\Software\Policies\Zeitgleich\TeamCraft` and the macOS configuration profile
//! `/Library/Managed Preferences/de.zeitgleich.teamcraft.plist`.
//!
//! `lock_server` is a security control, so only machine-wide locations an ordinary user cannot write
//! are read: never `HKCU`, never a per-user preferences file. The values are turned into the same
//! settings as `desktop.json` ([`ManagedFile`]); which source wins is decided in one place,
//! `ManagedServer::resolve` in [`super::servers`]. A malformed value is ignored with a log line (the
//! value itself is not logged), it never stops the app from starting.

use super::servers::ManagedFile;

/// The Windows value names; the macOS keys are `server_url` and `lock_server` like in `desktop.json`.
#[cfg(any(windows, test))]
const WINDOWS_SERVER_URL: &str = "ServerUrl";
#[cfg(any(windows, test))]
const WINDOWS_LOCK_SERVER: &str = "LockServer";
#[cfg(windows)]
const WINDOWS_KEY: &str = r"Software\Policies\Zeitgleich\TeamCraft";

/// The machine-wide profile written by a device management tool (macOS).
#[cfg(target_os = "macos")]
const MACOS_PROFILE: &str = "/Library/Managed Preferences/de.zeitgleich.teamcraft.plist";
/// A profile is a few lines; anything bigger is not one.
#[cfg(target_os = "macos")]
const MAX_PROFILE_BYTES: u64 = 64 * 1024;

/// A value of the policy store, reduced to what the settings can use.
#[cfg(any(windows, target_os = "macos", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
enum PolicyValue {
  Text(String),
  Number(i64),
  Bool(bool),
  /// Some other type, named for the log.
  Other(&'static str),
}

/// The policy store of this computer, if it names a server or a lock.
pub fn load() -> Option<ManagedFile> {
  #[cfg(windows)]
  {
    load_windows()
  }
  #[cfg(target_os = "macos")]
  {
    load_macos()
  }
  #[cfg(not(any(windows, target_os = "macos")))]
  {
    None
  }
}

/// Turns the two policy values into settings. `maybe_server_url` is the server; `maybe_lock` locks
/// the app to it. A value of the wrong type counts as not set; the lock takes `true`/`false` and the
/// numbers 1/0 (a registry DWORD is how a policy template writes a flag).
#[cfg(any(windows, target_os = "macos", test))]
fn settings_from(
  maybe_server_url: Option<PolicyValue>,
  maybe_lock: Option<PolicyValue>,
  names: (&str, &str),
) -> Option<ManagedFile> {
  let (url_name, lock_name) = names;
  let maybe_server_url = match maybe_server_url {
    Some(PolicyValue::Text(text)) => Some(text),
    Some(other) => {
      warn_malformed(url_name, &other, "text");
      None
    },
    None => None,
  };
  let is_locked = match maybe_lock {
    Some(PolicyValue::Bool(flag)) => flag,
    Some(PolicyValue::Number(0)) => false,
    Some(PolicyValue::Number(1)) => true,
    Some(other) => {
      warn_malformed(lock_name, &other, "true/false or 1/0");
      false
    },
    None => false,
  };
  if maybe_server_url.is_none() && !is_locked {
    return None;
  }
  Some(ManagedFile {
    maybe_server_url,
    lock_server: is_locked,
  })
}

#[cfg(any(windows, target_os = "macos", test))]
fn warn_malformed(name: &str, found: &PolicyValue, expected: &str) {
  let found = match found {
    PolicyValue::Text(_) => "text",
    PolicyValue::Number(_) => "a number",
    PolicyValue::Bool(_) => "true/false",
    PolicyValue::Other(kind) => kind,
  };
  tracing::warn!(name, found, expected, "ignoring a malformed managed policy value");
}

#[cfg(windows)]
fn load_windows() -> Option<ManagedFile> {
  use windows_registry::{LOCAL_MACHINE, Type};

  // Local machine only: HKCU is writable by the user, so it could not hold a lock.
  let Ok(key) = LOCAL_MACHINE.open(WINDOWS_KEY) else {
    return None;
  };
  let read = |name: &str| -> Option<PolicyValue> {
    let value = key.get_value(name).ok()?;
    Some(match value.ty() {
      Type::String | Type::ExpandString => {
        String::try_from(value).map_or(PolicyValue::Other("text that is not valid"), PolicyValue::Text)
      },
      Type::U32 => u32::try_from(value).map_or(PolicyValue::Other("a number"), |number| {
        PolicyValue::Number(i64::from(number))
      }),
      _ => PolicyValue::Other("another registry type"),
    })
  };
  settings_from(
    read(WINDOWS_SERVER_URL),
    read(WINDOWS_LOCK_SERVER),
    (WINDOWS_SERVER_URL, WINDOWS_LOCK_SERVER),
  )
}

#[cfg(target_os = "macos")]
fn load_macos() -> Option<ManagedFile> {
  use std::path::Path;

  let path = Path::new(MACOS_PROFILE);
  // Root-owned in a root-owned folder, like desktop.json; silent when there is no profile.
  if !super::servers::is_admin_owned(path) {
    return None;
  }
  let bytes = match std::fs::metadata(path) {
    Ok(metadata) if metadata.len() > MAX_PROFILE_BYTES => {
      tracing::warn!("ignoring a managed profile that is too large");
      return None;
    },
    Ok(_) => std::fs::read(path),
    Err(error) => Err(error),
  };
  match bytes {
    Ok(bytes) => settings_from_plist(&bytes),
    Err(error) => {
      tracing::warn!(%error, "could not read the managed profile");
      None
    },
  }
}

/// The settings in a configuration profile's property list (XML or binary).
#[cfg(target_os = "macos")]
fn settings_from_plist(bytes: &[u8]) -> Option<ManagedFile> {
  let value = match plist::Value::from_reader(std::io::Cursor::new(bytes)) {
    Ok(value) => value,
    Err(error) => {
      tracing::warn!(%error, "ignoring an unreadable managed profile");
      return None;
    },
  };
  let Some(dictionary) = value.as_dictionary() else {
    tracing::warn!("ignoring a managed profile that is not a dictionary");
    return None;
  };
  let read = |name: &str| {
    dictionary.get(name).map(|value| match value {
      plist::Value::String(text) => PolicyValue::Text(text.clone()),
      plist::Value::Boolean(flag) => PolicyValue::Bool(*flag),
      plist::Value::Integer(number) => number
        .as_signed()
        .map_or(PolicyValue::Other("a number"), PolicyValue::Number),
      _ => PolicyValue::Other("another property list type"),
    })
  };
  settings_from(
    read("server_url"),
    read("lock_server"),
    ("server_url", "lock_server"),
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  const NAMES: (&str, &str) = (WINDOWS_SERVER_URL, WINDOWS_LOCK_SERVER);

  fn text(value: &str) -> Option<PolicyValue> {
    Some(PolicyValue::Text(value.to_owned()))
  }

  #[test]
  fn a_server_and_a_lock_become_settings() {
    let settings = settings_from(text("team.example.com"), Some(PolicyValue::Number(1)), NAMES).unwrap();
    assert_eq!(settings.maybe_server_url.as_deref(), Some("team.example.com"));
    assert!(settings.lock_server);

    let settings = settings_from(text("team.example.com"), Some(PolicyValue::Bool(false)), NAMES).unwrap();
    assert!(!settings.lock_server);
    let settings = settings_from(text("team.example.com"), None, NAMES).unwrap();
    assert!(!settings.lock_server);
  }

  #[test]
  fn nothing_set_means_no_policy() {
    assert!(settings_from(None, None, NAMES).is_none());
    assert!(settings_from(None, Some(PolicyValue::Number(0)), NAMES).is_none());
  }

  #[test]
  fn malformed_values_are_ignored_not_fatal() {
    // A server of the wrong type is dropped; a valid lock next to it stays (and locks nothing).
    let settings = settings_from(Some(PolicyValue::Number(5)), Some(PolicyValue::Bool(true)), NAMES).unwrap();
    assert!(settings.maybe_server_url.is_none());
    assert!(settings.lock_server);
    // A lock that is not a flag does not lock: it is ignored, the server still applies.
    for bad in [
      PolicyValue::Number(2),
      PolicyValue::Number(-1),
      PolicyValue::Other("x"),
    ] {
      let settings = settings_from(text("team.example.com"), Some(bad), NAMES).unwrap();
      assert_eq!(settings.maybe_server_url.as_deref(), Some("team.example.com"));
      assert!(!settings.lock_server);
    }
    assert!(
      settings_from(
        Some(PolicyValue::Other("x")),
        Some(PolicyValue::Text("yes".into())),
        NAMES
      )
      .is_none()
    );
  }

  #[cfg(target_os = "macos")]
  mod macos {
    use super::*;

    fn profile(body: &str) -> Vec<u8> {
      format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>{body}</dict></plist>"
      )
      .into_bytes()
    }

    #[test]
    fn reads_a_configuration_profile() {
      let bytes =
        profile("<key>server_url</key><string>team.example.com</string><key>lock_server</key><true/>");
      let settings = settings_from_plist(&bytes).unwrap();
      assert_eq!(settings.maybe_server_url.as_deref(), Some("team.example.com"));
      assert!(settings.lock_server);

      let bytes = profile(
        "<key>server_url</key><string>team.example.com</string><key>lock_server</key><integer>1</integer>",
      );
      assert!(settings_from_plist(&bytes).unwrap().lock_server);
    }

    #[test]
    fn reads_a_binary_property_list() {
      let mut dictionary = plist::Dictionary::new();
      dictionary.insert("server_url".into(), "team.example.com".into());
      dictionary.insert("lock_server".into(), true.into());
      let mut bytes = Vec::new();
      plist::Value::Dictionary(dictionary)
        .to_writer_binary(&mut bytes)
        .unwrap();
      let settings = settings_from_plist(&bytes).unwrap();
      assert_eq!(settings.maybe_server_url.as_deref(), Some("team.example.com"));
      assert!(settings.lock_server);
    }

    #[test]
    fn a_broken_profile_is_ignored() {
      assert!(settings_from_plist(b"not a property list").is_none());
      assert!(settings_from_plist(b"").is_none());
      // An array instead of a dictionary.
      let bytes = b"<?xml version=\"1.0\"?><plist version=\"1.0\"><array/></plist>";
      assert!(settings_from_plist(bytes).is_none());
      // Wrong types and unrelated keys.
      let bytes = profile(
        "<key>server_url</key><array/><key>lock_server</key><string>true</string><key>other</key><true/>",
      );
      assert!(settings_from_plist(&bytes).is_none());
    }
  }
}
