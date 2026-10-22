//! Helpers for reading signals from code that runs both inside and outside reactive scopes.

use leptos::prelude::*;
use leptos::reactive::graph::Observer;

/// Reads a signal, subscribing the caller only when there is one. Component bodies and event handlers
/// have no observer; reading there is intentional and must not trigger the debug-build warning.
pub fn get_if_observed<T: Clone + Send + Sync + 'static>(signal: RwSignal<T>) -> T {
  if Observer::get().is_some() {
    signal.get()
  } else {
    signal.get_untracked()
  }
}
