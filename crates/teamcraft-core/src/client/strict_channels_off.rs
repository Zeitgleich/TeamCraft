//! Strict channels while the app doesn't follow them (e2ee.md §4.6.2, issue #1896): the web build
//! without the `strict-channels` feature (Cargo.toml). This device's leaves don't announce the
//! capability, so it is never in a strict group; the hooks `strict_channels.rs` gives the group
//! code are no-ops here, and the module stays out of the bundle.

use teamcraft_crypto::mls::{MlsError, StagedJoin};
use teamcraft_proto::{ConversationId, MlsMessageResponse};

use super::Client;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::store::LocalStore;
use crate::trust::PinnedDirectory;

/// Whether the app follows strict channels: not in this build.
pub(super) const ENABLED: bool = false;

/// Whether a welcome is opened ([`Client::check_strict_welcome`]): always, in this build.
pub(super) enum StrictWelcome {
  Open,
  #[expect(dead_code, reason = "only strict_channels.rs refuses a welcome")]
  Refused(MlsError),
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Whether the conversation's group is strict: never one this device is in, in this build.
  pub fn is_strict(&mut self, _conversation: ConversationId) -> Result<bool, CoreError> {
    Ok(false)
  }

  pub(super) fn is_strict_stopped(&mut self, _conversation: ConversationId) -> Result<bool, CoreError> {
    Ok(false)
  }

  pub(super) fn is_strict_rewind(&self, _conversation: ConversationId) -> bool {
    false
  }

  pub(super) fn persist_strict_stop_at(
    &mut self,
    _message: &MlsMessageResponse,
    _error: &MlsError,
  ) -> Result<(), CoreError> {
    Ok(())
  }

  pub(super) fn note_known_strict_roles(&mut self, _conversation: ConversationId) -> Result<(), CoreError> {
    Ok(())
  }

  pub(super) fn strict_rejoin_delay_ms(&mut self, _conversation: ConversationId) -> Result<i64, CoreError> {
    Ok(0)
  }

  pub(super) async fn check_strict_welcome(
    &mut self,
    _conversation: ConversationId,
    _staged: &StagedJoin,
    _directory: &PinnedDirectory,
  ) -> Result<StrictWelcome, CoreError> {
    Ok(StrictWelcome::Open)
  }
}
