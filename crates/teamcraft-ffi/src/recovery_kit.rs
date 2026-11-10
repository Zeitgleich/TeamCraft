//! The recovery kit on the phones (e2ee.md §6.5.4, issue #1701): the same key, kit page and check
//! as the web and desktop app. The key is made and kept here; the app shows its groups, saves the
//! page with the system's file picker and asks for one group (or the saved file) back before the
//! device enrolls with it ([`crate::TeamCraftCore::enroll_with_recovery_kit`]).

use rand_core::OsRng;
use teamcraft_client_api::recovery_kit::{self, KitTexts};
use teamcraft_crypto::recovery_key::RecoveryKey;

/// A file bigger than this is no kit (a kit is a few kilobytes); it isn't searched.
const MAX_KIT_FILE_BYTES: usize = 1024 * 1024;

/// A fresh recovery key on its way into a kit. It protects nothing until the device enrolls with it;
/// a new draft (after the app was closed) is a new key, and a kit saved from the old one is useless.
#[derive(uniffi::Object)]
pub struct RecoveryKitDraft {
  key: RecoveryKey,
}

/// The kit page's words in the app's language; the account, server and date are in `account_line`.
#[derive(uniffi::Record)]
pub struct RecoveryKitTexts {
  pub language: String,
  pub title: String,
  pub account_line: String,
  pub intro: String,
  pub key_label: String,
  pub keep_title: String,
  pub keep: Vec<String>,
  pub use_title: String,
  pub use_text: String,
  pub lost_title: String,
  pub lost_text: String,
  pub never_asked: String,
}

/// What a file opened at the kit's check holds.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum KitFileCheck {
  /// The kit of this draft's key.
  Matches,
  /// A kit, but of another key (an older kit, another account's).
  OtherKey,
  /// No recovery key in it.
  NoKey,
}

#[uniffi::export]
impl RecoveryKitDraft {
  #[uniffi::constructor]
  pub fn new() -> Self {
    Self {
      key: RecoveryKey::generate(&mut OsRng),
    }
  }

  /// The key as the user reads it: 8 groups of 4 characters.
  pub fn groups(&self) -> Vec<String> {
    self.key.groups().to_vec()
  }

  /// The kit page to save (e2ee.md §6.5.3).
  pub fn kit_html(&self, texts: RecoveryKitTexts) -> String {
    recovery_kit::kit_html(&self.key, &texts.into()).to_string()
  }

  /// Whether `typed` is group `index` (from 0), read as forgivingly as the key itself.
  pub fn is_group(&self, index: u32, typed: String) -> bool {
    usize::try_from(index).is_ok_and(|index| recovery_kit::is_group(&self.key, index, &typed))
  }

  /// What the file the user opened at the check holds, given as its text.
  pub fn check_file(&self, text: String) -> KitFileCheck {
    match key_in(&text) {
      Some(found) if self.key.is_same_key(&found) => KitFileCheck::Matches,
      Some(_) => KitFileCheck::OtherKey,
      None => KitFileCheck::NoKey,
    }
  }
}

impl RecoveryKitDraft {
  pub(crate) fn key(&self) -> RecoveryKey {
    self.key.duplicate()
  }
}

impl Default for RecoveryKitDraft {
  fn default() -> Self {
    Self::new()
  }
}

/// `teamcraft-recovery-kit-<host>.html`: names the server, never the key.
#[uniffi::export]
pub fn recovery_kit_file_name(server_host: String) -> String {
  recovery_kit::kit_file_name(&server_host)
}

/// The recovery key in a file the user picked to unlock with (a saved kit, or any text with the
/// key's machine form), as the user would type it; `None` when it holds none.
#[uniffi::export]
pub fn recovery_key_in_file(text: String) -> Option<String> {
  key_in(&text).map(|key| key.to_display().to_string())
}

fn key_in(text: &str) -> Option<RecoveryKey> {
  if text.len() > MAX_KIT_FILE_BYTES {
    return None;
  }
  RecoveryKey::find_in(text)
}

impl From<RecoveryKitTexts> for KitTexts {
  fn from(texts: RecoveryKitTexts) -> Self {
    Self {
      language: texts.language,
      title: texts.title,
      account_line: texts.account_line,
      intro: texts.intro,
      key_label: texts.key_label,
      keep_title: texts.keep_title,
      keep: texts.keep,
      use_title: texts.use_title,
      use_text: texts.use_text,
      lost_title: texts.lost_title,
      lost_text: texts.lost_text,
      never_asked: texts.never_asked,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_saved_kit_checks_as_this_draft_and_unlocks_with_its_key() {
    let draft = RecoveryKitDraft::new();
    let html = draft.kit_html(texts());
    assert_eq!(draft.check_file(html.clone()), KitFileCheck::Matches);
    assert_eq!(
      RecoveryKitDraft::new().check_file(html.clone()),
      KitFileCheck::OtherKey
    );
    assert_eq!(draft.check_file("no key here".into()), KitFileCheck::NoKey);
    let typed = recovery_key_in_file(html).unwrap();
    assert_eq!(typed, draft.key.to_display().as_str());
  }

  #[test]
  fn a_group_is_checked_against_the_draft() {
    let draft = RecoveryKitDraft::new();
    let groups = draft.groups();
    assert_eq!(groups.len(), 8);
    assert!(draft.is_group(3, groups[3].to_lowercase()));
    assert!(!draft.is_group(8, groups[3].clone()));
    assert!(!draft.is_group(u32::MAX, groups[3].clone()));
  }

  #[test]
  fn a_huge_file_is_not_searched() {
    let draft = RecoveryKitDraft::new();
    let mut text = draft.kit_html(texts());
    text.push_str(&" ".repeat(MAX_KIT_FILE_BYTES));
    assert_eq!(draft.check_file(text.clone()), KitFileCheck::NoKey);
    assert_eq!(recovery_key_in_file(text), None);
  }

  fn texts() -> RecoveryKitTexts {
    RecoveryKitTexts {
      language: "en".into(),
      title: "TeamCraft recovery kit".into(),
      account_line: "Account: Alice".into(),
      intro: "Keep this.".into(),
      key_label: "Recovery key".into(),
      keep_title: "Keep it safe".into(),
      keep: vec!["Print it.".into()],
      use_title: "When".into(),
      use_text: "Setup.".into(),
      lost_title: "Lost".into(),
      lost_text: "New kit.".into(),
      never_asked: "Never asked.".into(),
    }
  }
}
