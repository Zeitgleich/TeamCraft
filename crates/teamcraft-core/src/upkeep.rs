//! The periodic work of an app's sync loop besides the gateway: retrying what is waiting (outbox,
//! file uploads, link previews), committing staggered membership changes, syncing account data and
//! keeping the workspace roster and recovery keys current.
//!
//! Every shell's loop (the mobile runner, the web client, the desktop app) calls the same
//! [`Upkeep::tick`], so none of them silently skips a duty (docs/robustness-audit.md).

use std::time::Duration;

use crate::api::ServerApi;
use crate::client::Client;
use crate::store::LocalStore;

/// What runs on every tick, with the state of the work that runs less often.
#[derive(Clone, Debug)]
pub struct Upkeep {
  /// How often account data (saved items, drafts, saved searches) is pulled from the user's other
  /// devices; local changes and new content keys are pushed on the next tick regardless.
  account_sync: Duration,
  last_account_sync_ms: i64,
}

impl Upkeep {
  pub fn new(account_sync: Duration) -> Self {
    Self {
      account_sync,
      last_account_sync_ms: i64::MIN,
    }
  }

  /// Runs what is due. Returns whether the store may have changed (the UI re-reads it then).
  /// Failures are logged and retried on a later tick: offline, every step fails fast.
  pub async fn tick<A: ServerApi, S: LocalStore>(&mut self, client: &mut Client<A, S>) -> bool {
    let now = client.now_ms();
    let mut is_changed = false;
    let account_sync_ms = i64::try_from(self.account_sync.as_millis()).unwrap_or(i64::MAX);
    if client.is_key_bag_dirty() || now.saturating_sub(self.last_account_sync_ms) >= account_sync_ms {
      self.last_account_sync_ms = now;
      match client.sync_account_data().await {
        Ok(()) => is_changed = true,
        Err(error) => tracing::debug!(%error, "account data not synced yet"),
      }
      // Owners keep the signed roster fresh (a no-op for everyone else and while it is).
      if let Err(error) = client.refresh_workspace_signatures().await {
        tracing::debug!(%error, "roster not re-signed yet");
      }
      if let Err(error) = client.check_workspace_head().await {
        tracing::debug!(%error, "workspace head not checked yet");
      }
      // A rotated org recovery key: wrap the backup key to the new one (security review B4).
      if let Err(error) = client.rewrap_for_current_org_key().await {
        tracing::debug!(%error, "backup key not wrapped to the new org key yet");
      }
    }
    // Last seen of people who went offline since the last tick, in one request.
    match client.refresh_stale_last_seen().await {
      Ok(has_changed) => is_changed |= has_changed,
      Err(error) => tracing::debug!(%error, "reading last seen failed"),
    }
    // Key packages expire; a device that runs for weeks renews them in time (e2ee.md §4.3).
    if let Err(error) = client.renew_key_packages_if_due().await {
      tracing::debug!(%error, "key packages not renewed yet");
    }
    // Staggered commits (a colleague's new device) come due without a gateway event.
    if let Err(error) = client.apply_pending_changes().await {
      tracing::debug!(%error, "pending membership changes not applied yet");
    }
    is_changed | flush(client).await
  }
}

/// Sends what is waiting: link previews, then file uploads, then the outbox (a message with files
/// goes out once they are uploaded). Returns whether the store changed.
pub async fn flush<A: ServerApi, S: LocalStore>(client: &mut Client<A, S>) -> bool {
  let mut is_changed = false;
  if let Err(error) = client.generate_pending_previews().await {
    tracing::debug!(%error, "link previews not made");
  }
  match client.upload_pending().await {
    Ok(uploaded) => is_changed |= uploaded > 0,
    Err(error) => tracing::debug!(%error, "uploads did not finish"),
  }
  match client.flush_outbox().await {
    Ok(report) => {
      if report.pending > 0 || report.failed > 0 {
        tracing::debug!(
          sent = report.sent,
          pending = report.pending,
          failed = report.failed,
          "outbox"
        );
      }
      is_changed |= report.sent > 0;
    },
    Err(error) => tracing::debug!(%error, "outbox flush did not finish"),
  }
  is_changed
}
