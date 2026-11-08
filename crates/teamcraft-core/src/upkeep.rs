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

/// How often a device looks for scheduled messages to seal again: one local read, and a key change
/// usually comes with a gateway event long before their time.
#[cfg(feature = "ui")]
const SCHEDULED_CHECK_MS: i64 = 15_000;
/// How often a device reads the list because the gateway said it changed: a burst costs one read.
#[cfg(feature = "ui")]
const SCHEDULED_READ_PACE_MS: i64 = 5_000;
/// The longest a failed round of sealing waits for the next, doubling up to it from the interval.
/// Shorter than the server's last stretch, so that an entry that fails doesn't hold up another that
/// only needs one round before its time.
#[cfg(feature = "ui")]
const SCHEDULED_SEAL_BACKOFF_MAX_MS: i64 = 600_000;

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
  /// The server refused the key bag as too large and the user was told; once per run until an
  /// upload goes through again, since every retry fails the same way.
  has_reported_refusal: bool,
  /// When the roster and workspace head were last checked.
  last_local_check_ms: i64,
  /// When scheduled messages were last looked at, to notice this device's clock jumping back.
  #[cfg(feature = "ui")]
  last_scheduled_check_ms: i64,
  /// When they are looked at next.
  #[cfg(feature = "ui")]
  next_scheduled_check_ms: i64,
  /// Rounds of sealing that failed in a row: they pace the next one.
  #[cfg(feature = "ui")]
  scheduled_seal_failures: u32,
  /// When reading the list of scheduled messages last failed.
  #[cfg(feature = "ui")]
  maybe_scheduled_read_failed_ms: Option<i64>,
  /// When the list was last read because the gateway said it changed.
  #[cfg(feature = "ui")]
  last_scheduled_event_read_ms: i64,
  /// The gateway connection the list of scheduled messages was read for.
  #[cfg(feature = "ui")]
  maybe_scheduled_connection: Option<u64>,
  uploads: UploadMode,
}

/// Who runs the file uploads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UploadMode {
  /// The upkeep, in [`flush`]: the loop waits for every upload (the mobile runner).
  #[default]
  Inline,
  /// The host, beside its loop (`Client::start_upload`), so reads and sends are answered while a
  /// large file goes up (the web and desktop cores).
  Host,
}

impl Upkeep {
  pub fn new(interval: Duration) -> Self {
    Self {
      interval,
      maybe_synced_connection: None,
      last_attempt_ms: i64::MIN,
      has_failed: false,
      has_reported_refusal: false,
      last_local_check_ms: i64::MIN,
      #[cfg(feature = "ui")]
      last_scheduled_check_ms: i64::MIN,
      #[cfg(feature = "ui")]
      next_scheduled_check_ms: i64::MIN,
      #[cfg(feature = "ui")]
      scheduled_seal_failures: 0,
      #[cfg(feature = "ui")]
      maybe_scheduled_read_failed_ms: None,
      #[cfg(feature = "ui")]
      last_scheduled_event_read_ms: i64::MIN,
      #[cfg(feature = "ui")]
      maybe_scheduled_connection: None,
      uploads: UploadMode::Inline,
    }
  }

  /// Leaves the uploads to the host ([`UploadMode::Host`]).
  pub fn with_uploads(mut self, uploads: UploadMode) -> Self {
    self.uploads = uploads;
    self
  }

  /// Runs what is due. Returns whether the store may have changed (the UI re-reads it then).
  /// Failures are logged and retried on a later tick: offline, every step fails fast.
  pub async fn tick<A: ServerApi, S: LocalStore>(&mut self, client: &mut Client<A, S>) -> bool {
    let _step = crate::timing::step("upkeep_tick");
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
      // Self-adds no member answered (issue #907); a local read while none is due.
      match client.retry_due_self_adds().await {
        Ok(posted) => is_changed |= posted > 0,
        Err(error) => tracing::debug!(%error, "self-adds not posted again yet"),
      }
      // Messages whose keys didn't come in time read "not available" (issue #888); a local read
      // of one record per conversation that waits.
      match client.settle_missing_keys() {
        Ok(changed) => is_changed |= changed,
        Err(error) => tracing::debug!(%error, "messages waiting for keys not checked"),
      }
      // Devices registered by setups given up before this one finished (issue #415); a local read
      // while there are none.
      if let Err(error) = client.revoke_abandoned_devices().await {
        tracing::debug!(%error, "abandoned device registrations not revoked yet");
      }
    }
    // Roles follow the identity provider (e2ee.md §3.1.3); a local comparison while nothing changed.
    // Boxed: the round is deep, and this future would sit in every caller's stack.
    match Box::pin(client.run_role_signing()).await {
      Ok(changed) => is_changed |= changed,
      // Paced by its own backoff, so this says it now and then at most: a person's rights may wait on it.
      Err(error) => tracing::warn!(%error, "roles not signed yet"),
    }
    // Key packages expire; a device that runs for weeks renews them in time (e2ee.md §4.3).
    if let Err(error) = client.renew_key_packages_if_due().await {
      tracing::debug!(%error, "key packages not renewed yet");
    }
    // Staggered commits (a colleague's new device) come due without a gateway event.
    if let Err(error) = client.apply_pending_changes().await {
      tracing::debug!(%error, "pending membership changes not applied yet");
    }
    // Imported history before what this device held: one page per tick until it is all here, so
    // the loop stays responsive; nothing is read once it is.
    match client.page_imported_history().await {
      Ok(loaded) => is_changed |= loaded > 0,
      Err(error) => tracing::debug!(%error, "imported history not read further yet"),
    }
    #[cfg(feature = "ui")]
    {
      // On the heap: sealing catches groups up, too big a future for the frame of every tick's caller.
      is_changed |= Box::pin(self.tend_scheduled(client, now)).await;
    }
    let is_flushed = match self.uploads {
      UploadMode::Inline => flush(client).await,
      UploadMode::Host => flush_sends(client).await,
    };
    is_changed | is_flushed
  }

  /// Keeps this device's view of the scheduled messages (docs/scheduled-messages.md) and seals again
  /// what the group left behind. The list is read once per gateway connection (events bring changes
  /// in between); the check is a local read every [`SCHEDULED_CHECK_MS`] and no request while nothing
  /// is stale.
  #[cfg(feature = "ui")]
  async fn tend_scheduled<A: ServerApi, S: LocalStore>(
    &mut self,
    client: &mut Client<A, S>,
    now: i64,
  ) -> bool {
    let interval_ms = i64::try_from(self.interval.as_millis()).unwrap_or(i64::MAX);
    let mut is_changed = false;
    if let Some(connection) = client
      .gateway_connections()
      .filter(|connection| self.maybe_scheduled_connection != Some(*connection))
      // After a failed read not before the interval, like the account data.
      .filter(|_| {
        self
          .maybe_scheduled_read_failed_ms
          .is_none_or(|failed| now.abs_diff(failed) >= interval_ms.unsigned_abs())
      })
    {
      match client.refresh_scheduled().await {
        Ok(()) => {
          self.maybe_scheduled_connection = Some(connection);
          self.maybe_scheduled_read_failed_ms = None;
          self.seal_soon(now);
          is_changed = true;
        },
        Err(error) => {
          self.maybe_scheduled_read_failed_ms = Some(now);
          tracing::debug!(%error, "scheduled messages not read yet");
        },
      }
    }
    if now.abs_diff(self.last_scheduled_event_read_ms) >= SCHEDULED_READ_PACE_MS.unsigned_abs()
      && client.take_scheduled_list_stale()
    {
      self.last_scheduled_event_read_ms = now;
      match client.refresh_scheduled().await {
        Ok(()) => {
          self.seal_soon(now);
          is_changed = true;
        },
        Err(error) => {
          tracing::debug!(%error, "scheduled messages not read yet");
          client.note_scheduled_list_stale();
        },
      }
    }
    if !self.is_scheduled_check_due(now, interval_ms) {
      return is_changed;
    }
    self.last_scheduled_check_ms = now;
    match client.seal_stale_scheduled().await {
      Ok(sealed) => {
        self.scheduled_seal_failures = 0;
        self.next_scheduled_check_ms = now.saturating_add(SCHEDULED_CHECK_MS);
        is_changed || sealed > 0
      },
      Err(error) => {
        // They stay as they are, and the list says what that means. A group that can't be caught up
        // here isn't asked about every few seconds.
        tracing::debug!(%error, "scheduled messages not sealed again yet");
        self.next_scheduled_check_ms =
          now.saturating_add(seal_backoff_ms(interval_ms, self.scheduled_seal_failures));
        self.scheduled_seal_failures = self.scheduled_seal_failures.saturating_add(1);
        is_changed
      },
    }
  }

  /// Something changed on the server or a connection came back: what failed may work now.
  #[cfg(feature = "ui")]
  fn seal_soon(&mut self, now: i64) {
    self.scheduled_seal_failures = 0;
    self.next_scheduled_check_ms = self.next_scheduled_check_ms.min(now);
  }

  /// Due at its time, and at once when this device's clock jumped back by more than the interval: the
  /// time it waits for may be far away now.
  #[cfg(feature = "ui")]
  fn is_scheduled_check_due(&self, now: i64, interval_ms: i64) -> bool {
    now >= self.next_scheduled_check_ms || now < self.last_scheduled_check_ms.saturating_sub(interval_ms)
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
      if error.is_key_bag_refused() {
        // Drafts' thumbnails go first; the next round tries again without them.
        match client.shed_draft_thumbnails() {
          Ok(true) => return false,
          Ok(false) => {},
          Err(error) => tracing::warn!(%error, "drafts' thumbnails were not dropped"),
        }
        tracing::warn!("the server refuses the key bag as too large: keys and account data aren't backed up");
        let is_news = !std::mem::replace(&mut self.has_reported_refusal, true);
        if is_news {
          client.note_key_bag_refused();
        }
        return is_news;
      }
      tracing::debug!(%error, "account data not synced yet");
      return false;
    }
    self.has_failed = false;
    self.has_reported_refusal = false;
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
  if let Err(error) = client.check_channel_roles_heads().await {
    tracing::debug!(%error, "channel role heads not checked yet");
  }
  if let Err(error) = client.read_due_channel_roles().await {
    tracing::debug!(%error, "channel role logs due after a removal not read yet");
  }
  if let Err(error) = client.found_listed_channels().await {
    tracing::debug!(%error, "channels without a role log not founded yet");
  }
}

/// Sends what is waiting: problem reports into their outbox, file uploads, then link previews,
/// then the outbox (a message with files goes out once they are uploaded), then queued read
/// positions, acknowledgements, pins and presence. Returns whether the store changed.
pub async fn flush<A: ServerApi, S: LocalStore>(client: &mut Client<A, S>) -> bool {
  let mut is_changed = deliver_bug_reports(client).await;
  match client.upload_pending().await {
    Ok(uploaded) => is_changed |= uploaded > 0,
    Err(error) => tracing::debug!(%error, "uploads did not finish"),
  }
  is_changed | flush_sends(client).await
}

/// [`flush`] without the file uploads, for a host that runs them itself ([`UploadMode::Host`]).
/// Link previews are made first, so a message goes out with its preview.
pub async fn flush_sends<A: ServerApi, S: LocalStore>(client: &mut Client<A, S>) -> bool {
  // Before the host's uploads see the outbox: a report's files are written with its message.
  let mut is_changed = deliver_bug_reports(client).await;
  if let Err(error) = client.generate_pending_previews().await {
    tracing::debug!(%error, "link previews not made");
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

/// Puts queued problem reports into their group DMs (docs/bug-reports.md); whether any went.
async fn deliver_bug_reports<A: ServerApi, S: LocalStore>(client: &mut Client<A, S>) -> bool {
  match client.deliver_bug_reports().await {
    Ok(delivered) => delivered > 0,
    Err(error) => {
      tracing::debug!(%error, "problem reports not delivered yet");
      false
    },
  }
}

/// How long the next round waits after `failures` earlier failures in a row: the interval, doubling.
#[cfg(feature = "ui")]
fn seal_backoff_ms(interval_ms: i64, failures: u32) -> i64 {
  interval_ms
    .saturating_mul(1_i64.checked_shl(failures.min(16)).unwrap_or(i64::MAX))
    .min(SCHEDULED_SEAL_BACKOFF_MAX_MS.max(interval_ms))
}
