//! The built-in status presets for the phone apps (#2072): the one list of the client API, so
//! web, desktop, Android and iOS offer the same statuses. The apps show the text from their own
//! resources, found by [`StatusPresetInfo::id`].

use teamcraft_client_api::custom_status::BUILT_IN_PRESETS;

/// A status to pick with one tap.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct StatusPresetInfo {
  /// The end of the text's string resource (`status_preset_<id>`).
  pub id: String,
  pub emoji: String,
  /// The core's `StatusExpiry` variant name (`OneHour`, `Today`, `ThisWeek`, …).
  pub expiry: String,
}

/// The built-in presets in the order they are offered.
#[uniffi::export]
pub fn status_presets() -> Vec<StatusPresetInfo> {
  BUILT_IN_PRESETS
    .iter()
    .map(|preset| StatusPresetInfo {
      id: preset.id.to_owned(),
      emoji: preset.emoji.to_owned(),
      expiry: format!("{:?}", preset.expiry),
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn expiry_names_are_the_variant_names_the_apps_send_back() {
    let expiries: Vec<_> = status_presets().into_iter().map(|preset| preset.expiry).collect();
    assert_eq!(expiries, ["OneHour", "OneHour", "OneHour", "Today", "ThisWeek"]);
  }
}
