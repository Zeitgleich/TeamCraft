//! Search across the settings (issue #327): the label of every row and card, by section, matched
//! in the user's language. A hit opens its section and jumps to the row (`settings_rows::row_id`).

use crate::i18n::I18n;
use crate::state::SettingsTab;

/// Most hits listed at once; a short query matches a lot.
const MAX_HITS: usize = 30;

/// The labels (i18n keys) the search knows, by section. A row's label key is also its element ID
/// (`row_id`), so a new row only needs a line here to be found.
const ENTRIES: &[(SettingsTab, &str)] = &[
  (SettingsTab::Account, "settings-profile-details"),
  (SettingsTab::Account, "profile-avatar-change"),
  (SettingsTab::Account, "profile-title"),
  (SettingsTab::Account, "profile-department"),
  (SettingsTab::Account, "profile-phone"),
  (SettingsTab::Account, "profile-time-zone"),
  (SettingsTab::Account, "settings-profile-hours-show"),
  (SettingsTab::Appearance, "settings-theme"),
  (SettingsTab::Appearance, "settings-density"),
  (SettingsTab::Appearance, "settings-image-size"),
  (SettingsTab::Appearance, "settings-text-size"),
  (SettingsTab::Appearance, "settings-zoom"),
  (SettingsTab::Appearance, "settings-join-leave"),
  (SettingsTab::Appearance, "settings-status-in-messages"),
  (SettingsTab::Appearance, "settings-divider-dwell"),
  (SettingsTab::Notifications, "notif-master"),
  (SettingsTab::Notifications, "notif-test"),
  (SettingsTab::Notifications, "notif-desktop"),
  (SettingsTab::Notifications, "notif-preview"),
  (SettingsTab::Notifications, "notif-first-name"),
  (SettingsTab::Notifications, "notif-channel-mentions"),
  (SettingsTab::Notifications, "notif-confirm-channel"),
  (SettingsTab::Notifications, "notif-keywords"),
  (SettingsTab::Notifications, "notif-highlights"),
  (SettingsTab::Notifications, "notif-thread-replies"),
  (SettingsTab::Notifications, "notif-auto-follow"),
  (SettingsTab::Notifications, "notif-push"),
  (SettingsTab::Notifications, "notif-push-when"),
  (SettingsTab::Notifications, "notif-push-delay"),
  (SettingsTab::Notifications, "notif-uses-attention"),
  (SettingsTab::Notifications, "notif-attention-input"),
  (SettingsTab::Notifications, "notif-ringtone"),
  (SettingsTab::Notifications, "notif-meeting-reminder"),
  (SettingsTab::Notifications, "notif-meeting-start"),
  (SettingsTab::Notifications, "notif-dnd-enable"),
  (SettingsTab::Notifications, "notif-dnd-urgent"),
  (SettingsTab::Notifications, "notif-dnd-allow"),
  (SettingsTab::Notifications, "notif-per-conversation"),
  (SettingsTab::Preferences, "settings-app-language"),
  (SettingsTab::Preferences, "settings-send-mode"),
  (SettingsTab::Preferences, "settings-convert-emoticons"),
  (SettingsTab::Preferences, "settings-show-formatting"),
  (SettingsTab::Preferences, "settings-unreads-group"),
  (SettingsTab::Preferences, "settings-sidebar-grouped"),
  (SettingsTab::Preferences, "settings-sidebar-sort"),
  (SettingsTab::Preferences, "settings-autostart"),
  (SettingsTab::Preferences, "settings-close-to-tray"),
  (SettingsTab::Preferences, "settings-start-minimized"),
  (SettingsTab::Preferences, "settings-system-title-bar"),
  (SettingsTab::Keyboard, "shortcuts-title"),
  (SettingsTab::AudioVideo, "prejoin-camera"),
  (SettingsTab::AudioVideo, "prejoin-microphone"),
  (SettingsTab::AudioVideo, "prejoin-speaker"),
  (SettingsTab::AudioVideo, "calls-start-muted"),
  (SettingsTab::AudioVideo, "calls-start-camera-off"),
  (SettingsTab::AudioVideo, "prejoin-mirror"),
  (SettingsTab::AudioVideo, "calls-noise"),
  (SettingsTab::AudioVideo, "calls-echo"),
  (SettingsTab::AudioVideo, "calls-ptt"),
  (SettingsTab::AudioVideo, "calls-status-sounds"),
  (SettingsTab::AudioVideo, "calls-background"),
  (SettingsTab::Devices, "settings-device-this"),
  (SettingsTab::Devices, "settings-device-others"),
  (SettingsTab::Devices, "settings-device-link"),
  (SettingsTab::Devices, "settings-device-sign-out-others"),
  (SettingsTab::Security, "recovery-title"),
  (SettingsTab::Security, "security-verify"),
  (SettingsTab::Security, "security-my-key"),
  (SettingsTab::Security, "privacy-read-receipts"),
  (SettingsTab::Security, "privacy-typing"),
  (SettingsTab::Security, "privacy-last-seen"),
  (SettingsTab::Security, "privacy-away"),
  (SettingsTab::About, "settings-help-version"),
  (SettingsTab::About, "settings-help-copy-debug"),
  (SettingsTab::About, "settings-updates"),
  (SettingsTab::About, "about-notices"),
];

/// One search hit: where it is and what it is called.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsHit {
  pub tab: SettingsTab,
  /// The i18n key of the row's label; `None` for a section itself.
  pub maybe_label_key: Option<&'static str>,
  pub text: String,
}

/// The sections and rows whose name contains every word of `query` (any case).
pub fn search(i18n: I18n, query: &str) -> Vec<SettingsHit> {
  let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
  if words.is_empty() {
    return Vec::new();
  }
  let matches = |text: &str| {
    let text = text.to_lowercase();
    words.iter().all(|word| text.contains(word.as_str()))
  };
  let sections = SettingsTab::ALL.into_iter().filter_map(|tab| {
    let text = i18n.t(&section_label_key(tab));
    matches(&text).then_some(SettingsHit {
      tab,
      maybe_label_key: None,
      text,
    })
  });
  let rows = ENTRIES.iter().filter_map(|(tab, key)| {
    let text = i18n.t(key);
    matches(&text).then_some(SettingsHit {
      tab: *tab,
      maybe_label_key: Some(key),
      text,
    })
  });
  sections.chain(rows).take(MAX_HITS).collect()
}

/// The i18n key naming a section in the list.
pub fn section_label_key(tab: SettingsTab) -> String {
  format!("settings-tab-{}", tab.slug())
}

/// Every i18n key the search reads, for the test that they exist.
#[cfg(test)]
pub fn every_key() -> impl Iterator<Item = String> {
  SettingsTab::ALL
    .into_iter()
    .map(section_label_key)
    .chain(ENTRIES.iter().map(|(_, key)| (*key).to_owned()))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn every_section_is_searchable_by_a_row() {
    for tab in SettingsTab::ALL {
      assert!(ENTRIES.iter().any(|(candidate, _)| *candidate == tab), "{tab:?}");
    }
  }

  #[test]
  fn labels_are_listed_once() {
    let mut keys: Vec<&str> = ENTRIES.iter().map(|(_, key)| *key).collect();
    keys.sort_unstable();
    let count = keys.len();
    keys.dedup();
    assert_eq!(keys.len(), count);
  }
}
