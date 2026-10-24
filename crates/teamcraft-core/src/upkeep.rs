//! The periodic work of an app's sync loop besides the gateway: retrying what is waiting (outbox,
//! file uploads, link previews), committing staggered membership changes, syncing account data and
//! keeping the workspace roster and recovery keys current.
//!
//! Every shell's loop (the mobile runner, the web client, the desktop app) calls the same
//! [`Upkeep::tick`], so none of them silently skips a duty (docs/robustness-audit.md).
//!
//! Nothing here asks the server on a timer while all is well (issue #252): account data is read
//! once after each gateway connection (events in between may be lost) and whenever this device
//! changed it; `key_bag_update` events bring other devices' changes in between. The timer only
//! paces retries after a failure and local checks that rarely lead to a request.

use std::time::Duration;

use crate::api::ServerApi;
use crate::client::Client;
use crate::store::LocalStore;

/// What runs on every tick, with the state of the work that runs less often.
#[derive(Clone, Debug)]
pub struct Upkeep {
  /// How long a failed account sync waits before it is tried again, and how often the roster's
  /// age is checked locally (owners re-sign it before it expires).
  interval: Duration,
  /// The gateway connection (`LiveMarks::connections`) the account data was synced for.
  maybe_synced_connection: Option<u64>,
  /// When the account sync was last attempted, and whether that attempt failed.
  last_attempt_ms: i64,
  has_failed: bool,
  /// When the roster and workspace head were last checked.
  last_local_check_ms: i64,
}

impl Upkeep {
  pub fn new(interval: Duration) -> Self {
    Self {
      interval,
      maybe_synced_connection: None,
      last_attempt_ms: i64::MIN,
      has_failed: false,
      last_local_check_ms: i64::MIN,
    }
  }

  /// Runs what is due. Returns whether the store may have changed (the UI re-reads it then).
  /// Failures are logged and retried on a later tick: offline, every step fails fast.
  pub async fn tick<A: ServerApi, S: LocalStore>(&mut self, client: &mut Client<A, S>) -> bool {
    let now = client.now_ms();
    let interval_ms = i64::try_from(self.interval.as_millis()).unwrap_or(i64::MAX);
    let mut is_changed = false;
    if self.is_account_sync_due(client, now, interval_ms) {
      is_changed |= self.sync_account(client, now).await;
    }
    if now.saturating_sub(self.last_local_check_ms) >= interval_ms {
      self.last_local_check_ms = now;
      check_workspace(client).await;
      // Conversations a sync had to skip (a 403 right after a removal, a group in a bad state);
      // a local read that asks the server only for conversations that are marked.
      match client.resync_marked_conversations().await {
        Ok(caught_up) => is_changed |= caught_up > 0,
        Err(error) => tracing::debug!(%error, "skipped conversations not synced yet"),
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

  /// Due after a new gateway connection and after a local change; after a failed attempt not
  /// before `interval` has passed.
  fn is_account_sync_due<A: ServerApi, S: LocalStore>(
    &self,
    client: &Client<A, S>,
    now: i64,
    interval_ms: i64,
  ) -> bool {
    let is_wanted = client.is_key_bag_dirty()
      || client
        .gateway_connections()
        .is_some_and(|connection| self.maybe_synced_connection != Some(connection));
    is_wanted && (!self.has_failed || now.saturating_sub(self.last_attempt_ms) >= interval_ms)
  }

  async fn sync_account<A: ServerApi, S: LocalStore>(&mut self, client: &mut Client<A, S>, now: i64) -> bool {
    self.last_attempt_ms = now;
    let maybe_connection = client.gateway_connections();
    self.has_failed = true;
    if let Err(error) = client.sync_account_data().await {
      tracing::debug!(%error, "account data not synced yet");
      return false;
    }
    self.has_failed = false;
    self.maybe_synced_connection = maybe_connection;
    // A rotated org recovery key: wrap the backup key to the new one (security review B4). The
    // gateway does this when the roster changes; this catches a change missed while away.
    if let Err(error) = client.rewrap_for_current_org_key().await {
      tracing::debug!(%error, "backup key not wrapped to the new org key yet");
    }
    check_workspace(client).await;
    true
  }
}

/// Owners keep the signed roster fresh (a no-op for everyone else and while it is), and a roster
/// newer than the one verified is fetched. Both only ask the server when the local state says so.
async fn check_workspace<A: ServerApi, S: LocalStore>(client: &mut Client<A, S>) {
  if let Err(error) = client.refresh_workspace_signatures().await {
    tracing::debug!(%error, "roster not re-signed yet");
  }
  if let Err(error) = client.check_workspace_head().await {
    tracing::debug!(%error, "workspace head not checked yet");
  }
}

/// Sends what is waiting: link previews, then file uploads, then the outbox (a message with files
/// goes out once they are uploaded), then queued read positions, acknowledgements, pins and
/// presence. Returns whether the store changed.
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
  match client.flush_pending_actions().await {
    Ok(sent) => is_changed |= sent > 0,
    Err(error) => tracing::debug!(%error, "queued actions not sent"),
  }
  is_changed
}
