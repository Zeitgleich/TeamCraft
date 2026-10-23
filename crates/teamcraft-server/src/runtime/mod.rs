//! State that admins change while the server runs (admin.md §6): the settings in force, the mail
//! transport and the calendar store. Readers take a cheap snapshot (`Arc`) and never hold a lock
//! across an await; writers replace the whole value.

pub mod mail_status;
pub mod settings_key;

use std::sync::{Arc, PoisonError, RwLock};

use crate::state::Settings;

/// A value that can be swapped at run time, or be absent.
pub struct Slot<T: ?Sized> {
  inner: RwLock<Option<Arc<T>>>,
}

/// The settings in force: the configuration file's values with the console's changes on top.
pub struct SettingsCell {
  /// The configuration file's values: what a reset goes back to.
  base: RwLock<Arc<Settings>>,
  current: RwLock<Arc<Settings>>,
}

impl<T: ?Sized> Slot<T> {
  pub fn new(maybe_value: Option<Arc<T>>) -> Self {
    Self {
      inner: RwLock::new(maybe_value),
    }
  }

  pub fn get(&self) -> Option<Arc<T>> {
    // A writer that panicked left a complete value behind (assignments are not partial).
    self.inner.read().unwrap_or_else(PoisonError::into_inner).clone()
  }

  pub fn set(&self, maybe_value: Option<Arc<T>>) {
    *self.inner.write().unwrap_or_else(PoisonError::into_inner) = maybe_value;
  }

  pub fn is_set(&self) -> bool {
    self
      .inner
      .read()
      .unwrap_or_else(PoisonError::into_inner)
      .is_some()
  }
}

impl<T: ?Sized> Default for Slot<T> {
  fn default() -> Self {
    Self::new(None)
  }
}

impl SettingsCell {
  pub fn new(settings: Settings) -> Self {
    let settings = Arc::new(settings);
    Self {
      base: RwLock::new(Arc::clone(&settings)),
      current: RwLock::new(settings),
    }
  }

  /// The settings in force.
  pub fn get(&self) -> Arc<Settings> {
    self
      .current
      .read()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
  }

  /// The configuration file's settings, without the console's changes.
  pub fn base(&self) -> Arc<Settings> {
    self.base.read().unwrap_or_else(PoisonError::into_inner).clone()
  }

  /// Puts settings computed from the base in force.
  pub fn set_current(&self, settings: Settings) {
    *self.current.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(settings);
  }

  /// Replaces the base and the settings in force (tests that change the configuration).
  pub fn reset(&self, settings: Settings) {
    let settings = Arc::new(settings);
    *self.base.write().unwrap_or_else(PoisonError::into_inner) = Arc::clone(&settings);
    *self.current.write().unwrap_or_else(PoisonError::into_inner) = settings;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_slot_hands_out_the_value_set_last() {
    let slot: Slot<str> = Slot::new(Some(Arc::from("a")));
    let before = slot.get();
    slot.set(Some(Arc::from("b")));
    assert_eq!(before.as_deref(), Some("a"));
    assert_eq!(slot.get().as_deref(), Some("b"));
    slot.set(None);
    assert!(!slot.is_set());
  }

  #[test]
  fn the_base_survives_changes_of_the_current_settings() {
    let cell = SettingsCell::new(Settings::default());
    let mut changed = Settings::default();
    changed.integration.default_retention_days = 7;
    cell.set_current(changed);
    assert_eq!(cell.get().integration.default_retention_days, 7);
    assert_eq!(cell.base().integration.default_retention_days, 30);
  }
}
