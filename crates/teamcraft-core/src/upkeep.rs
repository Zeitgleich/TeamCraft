//! The periodic work of an app's sync loop besides the gateway: retrying what is waiting (outbox,
//! file uploads, link previews), committing staggered membership changes, syncing account data and
//! keeping the workspace roster and recovery keys current.
//!
//! Every shell's loop (the mobile runner, the web client, the desktop app) calls the same
//! [`Upkeep::tick`], so none of them silently skips a duty (docs/robustness-audit.md).
//!
//! The rounds only a workspace member's device has work in (signing roles and the roster, founding
//! channels' role logs, removing leavers) can be handed to the host: [`Upkeep::tick_shared`] leaves
//! them out and [`Upkeep::run_member_rounds`] runs them. The web client does that, so their code
//! loads with the signed-in app and a meeting guest's page never downloads it (docs/performance.md,
//! "Web bundle size"). Every other shell calls [`Upkeep::tick`], which runs them in place.
//!
//! Nothing here asks the server on a timer while all is well (issue #252): account data is read
//! once after each gateway connection (events in between may be lost) and whenever this device
//! changed it; `key_bag_update` events bring other devices' changes in between. The timer only
//! paces retries after a failure and local checks that rarely lead to a request.

use std::future::Future;
use std::time::Duration;

use crate::api::ServerApi;
use crate::client::{Client, ReaskTrigger};
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
  /// The gateway connection this device last asked again for keys on (issue #1878).
  maybe_key_reask_connection: Option<u64>,
  /// [`Self::tick_shared`] came to the workspace's signing (roster, channels' role logs) and left it
  /// to [`Self::run_member_rounds`], which runs it once and clears this.
  is_workspace_signing_due: bool,
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
      maybe_key_reask_connection: None,
      is_workspace_signing_due: false,
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

  /// Runs what is due, the member-only rounds included. Returns whether the store may have changed
  /// (the UI re-reads it then). Failures are logged and retried on a later tick: offline, every step
  /// fails fast.
  pub async fn tick<A: ServerApi, S: LocalStore>(&mut self, client: &mut Client<A, S>) -> bool {
    self.tick_with::<A, S, InPlace>(client).await
  }

  /// [`Self::tick`] without the member-only rounds, for a host that runs them itself with
  /// [`Self::run_member_rounds`] after every tick (the web client, from the signed-in app's module).
  /// Its code doesn't reference theirs ([`LeftToHost`]), so a split build can leave them out.
  pub async fn tick_shared<A: ServerApi, S: LocalStore>(&mut self, client: &mut Client<A, S>) -> bool {
    self.tick_with::<A, S, LeftToHost>(client).await
  }

  /// The rounds [`Self::tick_shared`] left out, on the same schedule as in [`Self::tick`]: the
  /// workspace's signing whenever the tick checked the workspace, roles and leavers on every call
  /// (each paced by its own state). Nothing on a meeting guest's device, which signs nothing
  /// (e2ee.md §4.6). Returns whether the store may have changed.
  pub async fn run_member_rounds<A: ServerApi, S: LocalStore>(&mut self, client: &mut Client<A, S>) -> bool {
    if !runs_member_rounds(client) {
      self.is_workspace_signing_due = false;
      return false;
    }
    if std::mem::take(&mut self.is_workspace_signing_due) {
      sign_workspace(client).await;
    }
    sign_and_remove(client).await
  }

  /// Whether the workspace's signing waits for [`Self::run_member_rounds`].
  pub fn is_workspace_signing_due(&self) -> bool {
    self.is_workspace_signing_due
  }

  /// The body of [`Self::tick`] and [`Self::tick_shared`].
  async fn tick_with<A: ServerApi, S: LocalStore, R: MemberRounds>(
    &mut self,
    client: &mut Client<A, S>,
  ) -> bool {
    let _step = crate::timing::step("upkeep_tick");
    let now = client.now_ms();
    let interval_ms = i64::try_from(self.interval.as_millis()).unwrap_or(i64::MAX);
    let mut is_changed = false;
    if self.is_account_sync_due(client, now, interval_ms) {
      is_changed |= self.sync_account::<A, S, R>(client, now).await;
    }
    let is_local_check_due = now.saturating_sub(self.last_local_check_ms) >= interval_ms;
    if is_local_check_due {
      self.last_local_check_ms = now;
      self.check_workspace::<A, S, R>(client).await;
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
    // On the heap: the requests are rare, and their future would add to every tick's frame.
    is_changed |= Box::pin(self.ask_again_for_keys(client, is_local_check_due)).await;
    is_changed |= R::run(client, MemberRound::RolesAndLeavers).await;
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
    // The local search index (issue #154): one bounded batch of what was written since; a read of
    // the queue while nothing waits. Searches find what waits anyway, so nothing shown changes.
    #[cfg(not(target_arch = "wasm32"))]
    if let Err(error) = client.index_search_step(crate::search_index::DEFAULT_STEP_MESSAGES) {
      tracing::warn!(%error, "the local search index was not brought up to date");
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

  /// Asks the members again where this device can't write (e2ee.md §4.6, "Asking again for a lost
  /// hand-over"): with the local checks, and at once on a new gateway connection (a reconnect, a
  /// resume, the app's start). Local reads while nothing waits.
  async fn ask_again_for_keys<A: ServerApi, S: LocalStore>(
    &mut self,
    client: &mut Client<A, S>,
    is_local_check_due: bool,
  ) -> bool {
    let maybe_connection = client.gateway_connections();
    let is_new_connection = maybe_connection.is_some() && maybe_connection != self.maybe_key_reask_connection;
    if !is_local_check_due && !is_new_connection {
      return false;
    }
    let trigger = if is_new_connection {
      ReaskTrigger::NewConnection
    } else {
      ReaskTrigger::Timer
    };
    self.maybe_key_reask_connection = maybe_connection;
    match client.ask_again_for_keys(trigger).await {
      Ok(posted) => posted > 0,
      Err(error) => {
        tracing::debug!(%error, "not every request for keys went out; asked again later");
        false
      },
    }
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

  async fn sync_account<A: ServerApi, S: LocalStore, R: MemberRounds>(
    &mut self,
    client: &mut Client<A, S>,
    now: i64,
  ) -> bool {
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
    self.check_workspace::<A, S, R>(client).await;
    true
  }

  /// Owners keep the signed roster fresh (a no-op for everyone else and while it is), a roster
  /// newer than the one verified is fetched, and channel admins found the role logs of channels from
  /// before signed roles. All only ask the server when the local state says so. Left to the host
  /// ([`LeftToHost`]), the signing waits for [`Self::run_member_rounds`].
  async fn check_workspace<A: ServerApi, S: LocalStore, R: MemberRounds>(
    &mut self,
    client: &mut Client<A, S>,
  ) {
    if !R::IS_IN_PLACE {
      self.is_workspace_signing_due = true;
    }
    R::run(client, MemberRound::Roster).await;
    if let Err(error) = client.check_workspace_head().await {
      tracing::debug!(%error, "workspace head not checked yet");
    }
    if let Err(error) = client.check_channel_roles_heads().await {
      tracing::debug!(%error, "channel role heads not checked yet");
    }
    if let Err(error) = client.read_due_channel_roles().await {
      tracing::debug!(%error, "channel role logs due after a removal not read yet");
    }
    R::run(client, MemberRound::Channels).await;
  }
}

/// Where [`Upkeep`]'s member-only rounds run. A type, not a flag: an `.await` behind a constant
/// `false` stays in the state machine, and with it the code it awaits.
trait MemberRounds {
  const IS_IN_PLACE: bool;

  /// Runs `round`; whether the store may have changed.
  fn run<A: ServerApi, S: LocalStore>(
    client: &mut Client<A, S>,
    round: MemberRound,
  ) -> impl Future<Output = bool>;
}

/// The rounds run in the tick ([`Upkeep::tick`]).
struct InPlace;

/// The host runs them ([`Upkeep::tick_shared`], then [`Upkeep::run_member_rounds`]).
struct LeftToHost;

/// A member-only round, at its place in the tick.
#[derive(Clone, Copy, Debug)]
enum MemberRound {
  /// Owners re-sign the roster, first in the workspace check.
  Roster,
  /// Channel admins found role logs, last in it.
  Channels,
  /// Roles and leavers, after the local checks.
  RolesAndLeavers,
}

impl MemberRounds for InPlace {
  const IS_IN_PLACE: bool = true;

  async fn run<A: ServerApi, S: LocalStore>(client: &mut Client<A, S>, round: MemberRound) -> bool {
    match round {
      MemberRound::Roster => {
        refresh_roster(client).await;
        false
      },
      MemberRound::Channels => {
        found_channels(client).await;
        false
      },
      MemberRound::RolesAndLeavers => sign_and_remove(client).await,
    }
  }
}

impl MemberRounds for LeftToHost {
  const IS_IN_PLACE: bool = false;

  fn run<A: ServerApi, S: LocalStore>(
    _client: &mut Client<A, S>,
    _round: MemberRound,
  ) -> impl Future<Output = bool> {
    std::future::ready(false)
  }
}

/// Whether a device has the member-only rounds to run: every device but a meeting guest's.
pub fn runs_member_rounds<A: ServerApi, S: LocalStore>(client: &Client<A, S>) -> bool {
  // An unreadable store counts as a member's: the rounds then fail and retry like any other step.
  !client.is_guest_device().unwrap_or(false)
}

/// The workspace's part of the member-only rounds, where [`Upkeep::check_workspace`] leaves it.
async fn sign_workspace<A: ServerApi, S: LocalStore>(client: &mut Client<A, S>) {
  refresh_roster(client).await;
  found_channels(client).await;
}

/// Owners sign the roster again before it expires (security review A6).
async fn refresh_roster<A: ServerApi, S: LocalStore>(client: &mut Client<A, S>) {
  if let Err(error) = client.refresh_workspace_signatures().await {
    tracing::debug!(%error, "roster not re-signed yet");
  }
}

/// Channel admins found the role logs of channels from before signed roles (e2ee.md §4.6.1).
async fn found_channels<A: ServerApi, S: LocalStore>(client: &mut Client<A, S>) {
  if let Err(error) = client.found_listed_channels().await {
    tracing::debug!(%error, "channels without a role log not founded yet");
  }
}

/// The member-only rounds of every tick: roles and leavers. Returns whether the store changed.
async fn sign_and_remove<A: ServerApi, S: LocalStore>(client: &mut Client<A, S>) -> bool {
  let mut is_changed = false;
  // Roles follow the identity provider (e2ee.md §3.1.3); a local comparison while nothing changed.
  // Boxed: the round is deep, and this future would sit in every caller's stack.
  match Box::pin(client.run_role_signing()).await {
    Ok(changed) => is_changed |= changed,
    // Paced by its own backoff, so this says it now and then at most: a person's rights may wait on it.
    Err(error) => tracing::warn!(%error, "roles not signed yet"),
  }
  // Leavers leave the groups (docs/provisioning.md §3); nothing while none is listed or due.
  match Box::pin(client.run_leaver_removal()).await {
    Ok(round) => is_changed |= round.removed > 0,
    Err(error) => tracing::warn!(%error, "leavers not removed from the groups yet"),
  }
  is_changed
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
