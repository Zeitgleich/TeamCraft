//! Entry point: configuration, logging, database, then serve.

use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use secrecy::{ExposeSecret, SecretString};
use teamcraft_db::{Database, DatabaseOptions, SnowflakeGenerator};
use teamcraft_ops::{DEFAULT_TIMEOUT, probe, probe_target, shutdown_signal};
use teamcraft_proto::PushProvider;
use teamcraft_server::auth::{
  HttpJwkSource, JwksCache, JwksCacheOptions, Revocations, RoleMapping, TokenValidator, ValidatorOptions,
};
use teamcraft_server::brand::{BrandInputs, BrandStore};
use teamcraft_server::config::{CONFIG_PATH_ENV, Config, DatabaseConfig};
use teamcraft_server::drain::Drain;
use teamcraft_server::gateway::{Hub, HubOptions};
use teamcraft_server::listener::bind_listener;
use teamcraft_server::logging;
use teamcraft_server::meetings::mailer::Mailer;
use teamcraft_server::meetings::{MeetingBackends, MeetingService};
use teamcraft_server::push::crypto::VapidKey;
use teamcraft_server::push::{
  APNS_PRODUCTION_URL, APNS_SANDBOX_URL, ApnsSender, ApnsSettings, FCM_BASE_URL, FcmSender, FcmSettings,
  GatewayBinding, GatewayClient, GatewaySettings, PushDispatcher, ServiceAccount, UnifiedPushSender, Vapid,
  WebPushSender, load_or_create_gateway_key, parse_gateway_key, spawn_retry_workers,
};
use teamcraft_server::rendezvous::RendezvousHub;
use teamcraft_server::router::router;
use teamcraft_server::runtime::mail_status::MailStatus;
use teamcraft_server::runtime::{SettingsCell, Slot, settings_key};
use teamcraft_server::security::RateLimits;
use teamcraft_server::services::calls::{self, CallService};
use teamcraft_server::services::local_accounts::LocalAuth;
use teamcraft_server::services::permission_scheme::{self, SchemeStore};
use teamcraft_server::services::presence::{self, refresh_presence};
use teamcraft_server::services::session::{self, PolicyHolder};
use teamcraft_server::services::{
  admin, attachments, backgrounds, brands, calendar_settings, email_digest, guests, key_packages,
  mail_settings, meeting_guests, oidc, retention, runtime_settings, urgent,
};
use teamcraft_server::state::{
  AppState, MeetingMailConfig, OidcSyncSettings, PushSettings, PushTtls, RetryPolicy, Settings,
  SignInSettings,
};
use teamcraft_server::storage::BlobStore;
use tracing::{info, warn};

const SESSION_SWEEP_INTERVAL: Duration = Duration::from_secs(30);
const BACKGROUND_SWEEP_INTERVAL: Duration = Duration::from_secs(600);
/// `[storage].max_attachment_bytes` when there is no storage (attachments are off then anyway).
const DEFAULT_MAX_ATTACHMENT_BYTES: u64 = 100 * 1024 * 1024;
/// Message partitions are created months ahead; checking twice a day is plenty.
const PARTITION_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(12 * 3600);
/// Backoff while the database is unreachable at startup.
const DATABASE_RETRY_FIRST: Duration = Duration::from_secs(1);
const DATABASE_RETRY_MAX: Duration = Duration::from_secs(15);

fn main() -> ExitCode {
  if std::env::args().nth(1).as_deref() == Some("healthcheck") {
    return healthcheck();
  }
  match run() {
    Ok(()) => ExitCode::SUCCESS,
    Err(error) => {
      // Logging may not be initialised yet, so report on stderr directly.
      let _ = writeln!(std::io::stderr(), "teamcraft-server failed: {error:#}");
      ExitCode::FAILURE
    },
  }
}

/// `teamcraft-server healthcheck [--ready]`: probes the running server's `/healthz` (or `/readyz`
/// with `--ready`) on its own listen address. For the container `HEALTHCHECK`; the image has no curl.
fn healthcheck() -> ExitCode {
  let path = if std::env::args().any(|arg| arg == "--ready") {
    "/readyz"
  } else {
    "/healthz"
  };
  let result = Config::load(config_path().as_deref())
    .map_err(anyhow::Error::from)
    .and_then(|config| {
      probe(probe_target(config.server.listen_addr), path, DEFAULT_TIMEOUT).map_err(anyhow::Error::from)
    });
  match result {
    Ok(()) => ExitCode::SUCCESS,
    Err(error) => {
      let _ = writeln!(std::io::stderr(), "unhealthy: {error:#}");
      ExitCode::FAILURE
    },
  }
}

#[tokio::main]
async fn run() -> anyhow::Result<()> {
  let mut config = Config::load(config_path().as_deref()).context("loading configuration")?;
  allow_web_connections(&mut config);
  config
    .web
    .validate()
    .map_err(anyhow::Error::msg)
    .context("checking [web]")?;
  let brands = BrandStore::new(BrandInputs {
    branding: config.branding.clone(),
    config_brands: config.brands.clone(),
    web: config.web.clone(),
    mail: config.mail.clone(),
  })
  .context("checking [branding] and [[brands]]")?;
  let maybe_settings_key =
    settings_key::load(&config.secrets).context("loading the settings key ([secrets])")?;
  logging::init(&config.logging)?;
  if config.mail.smtp.is_some()
    && config
      .meetings
      .as_ref()
      .is_some_and(|meetings| meetings.smtp.is_some())
  {
    warn!(
      "[meetings.smtp] is ignored: all mail goes through [mail.smtp] (or the admin console's mail server)"
    );
  }

  let database = connect_database(&config.database)
    .await
    .context("connecting to the database")?;
  database.migrate().await.context("running migrations")?;
  database
    .ensure_partitions(config.server.partitions_ahead_months)
    .await
    .context("creating message partitions")?;

  // Filled from the admin console's settings or the configuration once the database is read.
  let mailer: Arc<Slot<dyn Mailer>> = Arc::default();
  let state = AppState {
    database,
    validator: Arc::new(build_validator(&config)?),
    ids: Arc::new(SnowflakeGenerator::new(config.server.worker_id).context("invalid server.worker_id")?),
    push: Arc::new(build_push(&config)?),
    push_tracker: Arc::default(),
    revocations: Arc::new(Revocations::default()),
    hub: Arc::new(Hub::new(HubOptions {
      resume_window: Duration::from_secs(config.gateway.resume_window_secs),
      buffer_events: config.gateway.resume_buffer_events,
      buffer_bytes: config.gateway.resume_buffer_bytes,
      max_total_buffer_bytes: config.gateway.max_buffered_bytes,
      max_sessions_per_device: config.gateway.max_sessions_per_device,
      max_sessions_per_user: config.gateway.max_sessions_per_user,
      attention_lease: Duration::from_secs(config.notifications.attention_lease_secs),
      desktop_activity_window: Duration::from_secs(config.notifications.desktop_activity_secs),
    })),
    rendezvous: Arc::new(RendezvousHub::new(&config.rendezvous)),
    drain: Arc::default(),
    maybe_blob_store: config
      .storage
      .as_ref()
      .map(BlobStore::new)
      .transpose()
      .context("configuring attachment storage")?
      .map(Arc::new),
    maybe_calls: config
      .calls
      .as_ref()
      .map(CallService::from_config)
      .transpose()
      .context("configuring calls")?
      .map(Arc::new),
    maybe_meetings: build_meetings(&config, &mailer)?.map(Arc::new),
    maybe_local: build_local_auth(&config)?.map(Arc::new),
    mailer,
    mail_status: Arc::new(MailStatus::default()),
    maybe_settings_key: maybe_settings_key.map(Arc::new),
    settings_lock: Arc::default(),
    policy: Arc::new(PolicyHolder::default()),
    scheme: Arc::new(SchemeStore::default()),
    settings: Arc::new(SettingsCell::new(Settings {
      maybe_oidc_sync: build_oidc_sync(&config)?,
      policy: config.policy.clone(),
      limits: config.limits.clone(),
      gateway: config.gateway.clone(),
      rendezvous: config.rendezvous.clone(),
      retention: config.retention.clone(),
      notifications: config.notifications.clone(),
      integration: config.integration.clone(),
      web: config.web.clone(),
      security: config.security.clone(),
      guests: config.guests.clone(),
      mail: config.mail.clone(),
      local_accounts: config.local_accounts.clone(),
      sign_in_page: config.sign_in_page.clone(),
      presence: config.presence.clone(),
      shares: config.shares.clone(),
      maybe_organization_claim: config.oidc.maybe_organization_claim.clone(),
      sign_in: SignInSettings::new(
        config.oidc.issuer.clone(),
        config.oidc.client_id.clone(),
        &config.oidc.app_scopes,
        config.oidc.maybe_app_redirect_uris.clone(),
      ),
      branding: config.branding.clone(),
      brands: config.brands.clone(),
      push: push_settings(&config),
      max_attachment_bytes: config
        .storage
        .as_ref()
        .map_or(DEFAULT_MAX_ATTACHMENT_BYTES, |storage| {
          storage.max_attachment_bytes
        }),
      meeting_mail: config
        .meetings
        .as_ref()
        .map_or_else(MeetingMailConfig::default, |meetings| MeetingMailConfig {
          max_invitees: meetings.max_invitees,
          texts: meetings.mail_texts.clone(),
        }),
      maybe_mail_seed: config.mail.smtp.clone().or_else(|| {
        config
          .meetings
          .as_ref()
          .and_then(|meetings| meetings.smtp.clone())
      }),
      maybe_calendar_seed: config
        .meetings
        .as_ref()
        .and_then(|meetings| meetings.caldav.clone()),
    })),
    rate_limits: Arc::new(RateLimits::new(&config.security.rate_limits)),
    brands: Arc::new(brands),
    image_decodes: Arc::new(tokio::sync::Semaphore::new(
      config.limits.max_concurrent_image_decodes.max(1),
    )),
  };
  load_runtime_settings(&state).await?;
  spawn_session_sweeper(state.clone());
  if state.settings.get().maybe_oidc_sync.is_some() {
    spawn_oidc_sync(
      state.clone(),
      Duration::from_secs(config.oidc.sync.initial_delay_secs),
      Duration::from_secs(config.oidc.sync.interval_secs),
    );
  }
  if config.retention.is_enabled {
    spawn_retention_job(state.clone());
  }
  permission_scheme::load_scheme(&state)
    .await
    .context("loading the permission scheme")?;
  session::load_policy(&state)
    .await
    .context("loading the workspace policy")?;
  spawn_urgent_scheduler(state.clone());
  spawn_email_digests(state.clone());
  spawn_account_housekeeping(state.clone());
  spawn_background_sweeper(state.clone());
  spawn_partition_maintenance(state.clone(), config.server.partitions_ahead_months);
  spawn_derived_presence(state.clone());
  spawn_call_roster_sweeper(state.clone());
  // Before anybody asks for a join token: calls that ran on through this restart are taken over,
  // not opened a second time next to themselves (at most the SFU control timeout).
  calls::adopt_running_calls(&state).await;
  // Wake-ups that failed for a temporary reason, also those queued before a restart.
  spawn_retry_workers(&state);

  let listener = bind_listener(config.server.listen_addr)
    .with_context(|| format!("binding {}", config.server.listen_addr))?;
  info!(addr = %config.server.listen_addr, "teamcraft-server listening");
  let drain = state.drain.clone();
  let drain_timeout = Duration::from_secs(config.gateway.drain_timeout_secs);
  axum::serve(
    listener,
    router(state).into_make_service_with_connect_info::<SocketAddr>(),
  )
  .with_graceful_shutdown(begin_drain(drain.clone()))
  .await?;
  // Upgraded WebSockets outlive the HTTP server: give them a bounded time to send their 1012.
  let open_sockets = drain.open_sockets();
  if drain.wait_closed(drain_timeout).await {
    info!(open_sockets, "shut down");
  } else {
    warn!(
      left_open = drain.open_sockets(),
      "shut down before every WebSocket was closed"
    );
  }
  Ok(())
}

/// Resolves on SIGTERM or Ctrl-C after starting the drain: readiness fails at once and every
/// gateway and relay socket is closed with 1012, while the HTTP server finishes what it serves.
async fn begin_drain(drain: Arc<Drain>) {
  shutdown_signal().await;
  info!(
    open_sockets = drain.open_sockets(),
    "shutting down, closing WebSockets"
  );
  drain.begin();
}

/// Connects to Postgres, waiting up to `startup_wait_secs` while it is unreachable (it may be
/// restarting, or start after the server): exiting at once would make a restart policy loop.
/// Configuration errors (wrong password, unknown database) fail at once.
async fn connect_database(config: &DatabaseConfig) -> Result<Database, teamcraft_db::DbError> {
  let options = DatabaseOptions {
    max_connections: config.max_connections,
    acquire_timeout: config.acquire_timeout(),
  };
  let give_up_at = tokio::time::Instant::now() + config.startup_wait();
  let mut delay = DATABASE_RETRY_FIRST;
  loop {
    match Database::connect(config.url.expose_secret(), &options).await {
      Ok(database) => return Ok(database),
      Err(error) if error.is_transient() && tokio::time::Instant::now() + delay < give_up_at => {
        warn!(%error, retry_in_secs = delay.as_secs(), "the database is unreachable, retrying");
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(DATABASE_RETRY_MAX);
      },
      Err(error) => return Err(error),
    }
  }
}

/// Removes deleted and abandoned call backgrounds and unfinished attachments once their upload URLs
/// have expired.
fn spawn_background_sweeper(state: AppState) {
  tokio::spawn(async move {
    let mut interval = tokio::time::interval(BACKGROUND_SWEEP_INTERVAL);
    loop {
      interval.tick().await;
      let cutoff = backgrounds::sweep_cutoff(&state, time::OffsetDateTime::now_utc());
      match backgrounds::sweep(&state, cutoff).await {
        Ok(0) => {},
        Ok(swept) => info!(swept, "swept deleted call backgrounds"),
        Err(error) => warn!(%error, "sweeping call backgrounds failed"),
      }
      match attachments::sweep_unfinished(&state, cutoff).await {
        Ok(0) => {},
        Ok(swept) => info!(swept, "swept unfinished attachments"),
        Err(error) => warn!(%error, "sweeping unfinished attachments failed"),
      }
      match meeting_guests::sweep_expired_guests(&state).await {
        Ok(0) => {},
        Ok(ended) => info!(ended, "ended sessions of guests whose meeting is over"),
        Err(error) => warn!(%error, "ending expired guest sessions failed"),
      }
    }
  });
}

/// Ends the calls of an SFU that stopped sending its roster (#684), a few times per
/// `[calls] roster_timeout_secs`. Only with calls and SFU reports configured.
fn spawn_call_roster_sweeper(state: AppState) {
  let Some(period) = state
    .maybe_calls
    .as_ref()
    .filter(|calls| calls.is_reporting_enabled())
    .map(|calls| calls.roster_sweep_interval())
  else {
    return;
  };
  tokio::spawn(async move {
    let mut interval = tokio::time::interval(period);
    loop {
      interval.tick().await;
      calls::expire_silent_sfu(&state).await;
    }
  });
}

/// Keeps message partitions created ahead, whether or not the retention job runs: a server that
/// runs for longer than `months_ahead` would otherwise write messages into the default partition,
/// and creating the partition for that month then fails, also on the next start.
fn spawn_partition_maintenance(state: AppState, months_ahead: u32) {
  tokio::spawn(async move {
    let mut interval = tokio::time::interval(PARTITION_MAINTENANCE_INTERVAL);
    // The first tick fires at once; startup has just created the partitions.
    interval.tick().await;
    loop {
      interval.tick().await;
      if let Err(error) = state.database.ensure_partitions(months_ahead).await {
        warn!(%error, "creating message partitions ahead failed");
      }
    }
  });
}

/// Drops gateway sessions nobody resumed within the window and updates presence for their users.
fn spawn_session_sweeper(state: AppState) {
  tokio::spawn(async move {
    let mut interval = tokio::time::interval(SESSION_SWEEP_INTERVAL);
    loop {
      interval.tick().await;
      match presence::expire_statuses(&state, time::OffsetDateTime::now_utc()).await {
        Ok(_) => {},
        Err(error) => warn!(%error, "expiring custom statuses failed"),
      }
      for user in state.hub.sweep_expired() {
        if let Err(error) = refresh_presence(&state, user).await {
          warn!(%error, "presence refresh failed");
        }
      }
    }
  });
}

/// Keeps derived presence current: "in a meeting" / "in a call" for online users (asking the
/// calendar server now and then) and the away timeout.
fn spawn_derived_presence(state: AppState) {
  tokio::spawn(async move {
    let settings = state.settings.get().presence.clone();
    let mut interval = tokio::time::interval(Duration::from_secs(settings.auto_status_interval_secs.max(1)));
    let poll_every = Duration::from_secs(settings.calendar_poll_interval_secs.max(1));
    let mut maybe_last_poll: Option<tokio::time::Instant> = None;
    loop {
      interval.tick().await;
      let is_poll_due = maybe_last_poll.is_none_or(|at| at.elapsed() >= poll_every);
      if is_poll_due {
        maybe_last_poll = Some(tokio::time::Instant::now());
      }
      if let Some(minutes) = state.policy.get().presence.effective_away_timeout_minutes() {
        let cutoff = tokio::time::Instant::now().checked_sub(Duration::from_secs(u64::from(minutes) * 60));
        if let Some(cutoff) = cutoff
          && let Err(error) = presence::demote_idle_devices(&state, cutoff).await
        {
          warn!(%error, "applying the away timeout failed");
        }
      }
      if let Err(error) =
        presence::refresh_derived_statuses(&state, time::OffsetDateTime::now_utc(), is_poll_due).await
      {
        warn!(%error, "refreshing derived statuses failed");
      }
    }
  });
}

/// Runs the retention job periodically (deletes what is past retention, tops up partitions).
fn spawn_retention_job(state: AppState) {
  tokio::spawn(async move {
    let settings = state.settings.get().retention.clone();
    tokio::time::sleep(Duration::from_secs(settings.initial_delay_secs)).await;
    let mut interval = tokio::time::interval(Duration::from_secs(settings.interval_secs.max(1)));
    loop {
      interval.tick().await;
      if let Err(error) = retention::run_once(&state, time::OffsetDateTime::now_utc()).await {
        warn!(%error, "retention run failed");
      }
    }
  });
}

/// The web client talks to the identity provider, the SFU and the storage: its policy allows
/// exactly those origins (and the ones listed in `web.connect_origins`), not all of https.
fn allow_web_connections(config: &mut Config) {
  let mut urls = vec![config.oidc.issuer.as_str()];
  if let Some(calls) = &config.calls {
    urls.push(&calls.sfu_url);
  }
  let mut urls: Vec<String> = urls.into_iter().map(str::to_owned).collect();
  if let Some(storage) = &config.storage {
    urls.push(storage.client_url());
  }
  config
    .web
    .allow_connections_to(&urls.iter().map(String::as_str).collect::<Vec<_>>());
}

/// Local guest accounts, if enabled.
fn build_local_auth(config: &Config) -> anyhow::Result<Option<LocalAuth>> {
  if !config.local_accounts.is_enabled {
    return Ok(None);
  }
  LocalAuth::new(&config.local_accounts)
    .map(Some)
    .context("configuring local accounts")
}

/// The meeting feature, if `[meetings]` is configured. Its mail goes through the server's mail
/// server; the calendar is put in place with the other runtime settings.
fn build_meetings(config: &Config, mailer: &Arc<Slot<dyn Mailer>>) -> anyhow::Result<Option<MeetingService>> {
  let Some(meetings) = &config.meetings else {
    return Ok(None);
  };
  MeetingService::new(
    meetings,
    MeetingBackends {
      calendar: Arc::default(),
      mailer: Arc::clone(mailer),
    },
  )
  .map(Some)
  .context("configuring meetings")
}

/// Puts the admin console's settings on top of the configuration: instance settings, mail server,
/// calendar and brands.
async fn load_runtime_settings(state: &AppState) -> anyhow::Result<()> {
  runtime_settings::load(state)
    .await
    .context("loading the instance settings")?;
  mail_settings::load(state)
    .await
    .context("loading the mail server settings")?;
  calendar_settings::load(state)
    .await
    .context("loading the calendar settings")?;
  brands::load(state).await.context("loading the brands")?;
  Ok(())
}

/// Removes guests whose membership expired (and tells their sponsors), forgets old sessions,
/// removes long-deactivated users from all chats (when an admin set a period) and deletes key
/// packages that expire too soon to be handed out.
fn spawn_account_housekeeping(state: AppState) {
  tokio::spawn(async move {
    let period = Duration::from_secs(state.settings.get().guests.expiry_check_interval_secs.max(1));
    let mut interval = tokio::time::interval(period);
    loop {
      interval.tick().await;
      let now = time::OffsetDateTime::now_utc();
      if let Err(error) = guests::expire_once(&state, now).await {
        warn!(%error, "expiring guests failed");
      }
      if let Err(error) = session::forget_stale_sessions(&state, now).await {
        warn!(%error, "forgetting stale sessions failed");
      }
      match admin::sweep_deactivated_users(&state, now).await {
        Ok(0) => {},
        Ok(removed) => info!(removed, "removed long-deactivated users from all chats"),
        Err(error) => warn!(%error, "removing deactivated users from chats failed"),
      }
      match admin::prune_deleted_conversations(&state, now).await {
        Ok(0) => {},
        Ok(pruned) => info!(pruned, "forgot old deleted direct messages"),
        Err(error) => warn!(%error, "forgetting deleted direct messages failed"),
      }
      match key_packages::sweep_expired(&state, now).await {
        Ok(0) => {},
        Ok(deleted) => info!(deleted, "deleted expiring key packages"),
        Err(error) => warn!(%error, "deleting expiring key packages failed"),
      }
      match state.database.delete_expired_create_keys().await {
        Ok(0) => {},
        Ok(deleted) => info!(deleted, "deleted expired idempotency keys"),
        Err(error) => warn!(%error, "deleting expired idempotency keys failed"),
      }
    }
  });
}

/// Pushes urgent direct messages again until they are acknowledged (PLAN.md §5).
fn spawn_urgent_scheduler(state: AppState) {
  tokio::spawn(async move {
    let period = Duration::from_secs(
      state
        .settings
        .get()
        .notifications
        .urgent_scheduler_interval_secs
        .max(1),
    );
    let mut interval = tokio::time::interval(period);
    loop {
      interval.tick().await;
      if let Err(error) = urgent::run_due(&state, time::OffsetDateTime::now_utc()).await {
        warn!(%error, "urgent reminders failed");
      }
    }
  });
}

/// Mails the digests of missed mentions that are due (PLAN.md §5). Runs always: admins can switch
/// the digest on and off in the console, and a pass with it off does nothing.
fn spawn_email_digests(state: AppState) {
  tokio::spawn(async move {
    let period = Duration::from_secs(
      state
        .settings
        .get()
        .notifications
        .email_digest
        .scheduler_interval_secs
        .max(1),
    );
    let mut interval = tokio::time::interval(period);
    loop {
      interval.tick().await;
      if let Err(error) = email_digest::run_due(&state, time::OffsetDateTime::now_utc()).await {
        warn!(%error, "mention digests failed");
      }
    }
  });
}

/// Settings of the OIDC sync, if enabled and complete.
fn build_oidc_sync(config: &Config) -> anyhow::Result<Option<OidcSyncSettings>> {
  let oidc = &config.oidc;
  if !oidc.sync.is_enabled {
    return Ok(None);
  }
  let secret = oidc
    .sync
    .maybe_client_secret
    .clone()
    .context("oidc.sync.client_secret is required")?;
  let admin_url = match &oidc.sync.maybe_admin_url {
    Some(url) => url.clone(),
    None => teamcraft_server::services::oidc::admin_url_from_issuer(&oidc.issuer)
      .context("oidc.sync.admin_url is required: the issuer is not <base>/realms/<realm>")?,
  };
  Ok(Some(OidcSyncSettings {
    issuer: oidc.issuer.clone(),
    token_url: format!(
      "{}/protocol/openid-connect/token",
      oidc.issuer.trim_end_matches('/')
    ),
    admin_url,
    client_id: oidc.client_id.clone(),
    server_client: oidc.server_client.clone(),
    client_secret: secret,
    roles: oidc.roles.clone(),
    page_size: oidc.sync.page_size,
    sync_groups: oidc.sync.sync_groups,
  }))
}

/// Compares users with the identity provider periodically.
fn spawn_oidc_sync(state: AppState, initial_delay: Duration, interval: Duration) {
  tokio::spawn(async move {
    tokio::time::sleep(initial_delay).await;
    let mut ticker = tokio::time::interval(interval.max(Duration::from_secs(1)));
    loop {
      ticker.tick().await;
      if let Err(error) = oidc::sync_once(&state).await {
        warn!(%error, "oidc sync failed");
      }
    }
  });
}

/// The push senders: each provider whose credentials are configured, UnifiedPush unless disabled.
fn build_push(config: &Config) -> anyhow::Result<PushDispatcher> {
  let push = &config.push;
  let timeout = Duration::from_secs(push.request_timeout_secs);
  let mut dispatcher = PushDispatcher::new(push.max_concurrent_sends)
    .with_max_concurrent_sends_per_user(push.max_concurrent_sends_per_user);
  if let Some(private_key) = &push.maybe_vapid_private_key {
    let vapid = VapidKey::from_base64url(private_key.expose_secret()).context("push.vapid_private_key")?;
    let public_key = vapid.public_key_base64url();
    info!(%public_key, "Web Push enabled (applicationServerKey)");
    let sender = WebPushSender::new(
      Vapid::new(vapid, push.vapid_subject.clone()),
      timeout,
      push.allow_private_endpoints,
    )
    .context("building the Web Push client")?;
    dispatcher = dispatcher
      .with_sender(&PushProvider::WebPush, Arc::new(sender))
      .with_web_public_key(public_key);
  }
  let maybe_gateway = build_gateway(config)?;
  if let Some(settings) = apns_settings(config)? {
    let sender = ApnsSender::new(settings).context("push.apns")?;
    info!("APNs enabled (direct)");
    dispatcher = dispatcher.with_sender(&PushProvider::Apns, Arc::new(sender));
  } else if let Some(gateway) = &maybe_gateway {
    dispatcher = dispatcher.with_sender(&PushProvider::Apns, Arc::new(gateway.sender(PushProvider::Apns)));
  }
  if let Some(json) = secret_from_config(
    push.fcm.maybe_service_account.as_ref(),
    push.fcm.maybe_service_account_path.as_deref(),
    "push.fcm.service_account",
  )? {
    let account = ServiceAccount::from_json(json.expose_secret()).context("push.fcm.service_account")?;
    let base_url = push
      .fcm
      .maybe_base_url
      .clone()
      .unwrap_or_else(|| FCM_BASE_URL.into());
    require_tls(&base_url, "push.fcm.base_url", push.allow_private_endpoints)?;
    require_tls(
      &account.token_uri,
      "token_uri of push.fcm.service_account",
      push.allow_private_endpoints,
    )?;
    let sender = FcmSender::new(FcmSettings {
      account,
      base_url,
      timeout,
    })
    .context("push.fcm.service_account")?;
    info!("FCM enabled (direct)");
    dispatcher = dispatcher.with_sender(&PushProvider::Fcm, Arc::new(sender));
  } else if let Some(gateway) = &maybe_gateway {
    dispatcher = dispatcher.with_sender(&PushProvider::Fcm, Arc::new(gateway.sender(PushProvider::Fcm)));
  }
  if let Some(gateway) = &maybe_gateway {
    let providers: Vec<PushProvider> = [PushProvider::Apns, PushProvider::Fcm]
      .into_iter()
      .filter(|provider| dispatcher.is_gateway(provider))
      .collect();
    if !providers.is_empty() {
      dispatcher = dispatcher.with_gateway_binding(GatewayBinding {
        url: gateway.url().to_owned(),
        server_id: gateway.server_id().to_owned(),
        providers,
      });
    }
  }
  if push.unified_push.is_enabled {
    // Distributors may check a VAPID token; the same key as Web Push signs it when there is one.
    let maybe_vapid = match &push.maybe_vapid_private_key {
      Some(private_key) => Some(Vapid::new(
        VapidKey::from_base64url(private_key.expose_secret()).context("push.vapid_private_key")?,
        push.vapid_subject.clone(),
      )),
      None => None,
    };
    let sender = UnifiedPushSender::new(maybe_vapid, timeout, push.allow_private_endpoints)
      .context("building the UnifiedPush client")?;
    dispatcher = dispatcher.with_sender(&PushProvider::UnifiedPush, Arc::new(sender));
  }
  Ok(dispatcher)
}

/// The push gateway client, if `push.gateway.url` is set. The first push registers the server key
/// anyway; registering at startup surfaces a wrong URL or a refusing gateway in the log early.
fn build_gateway(config: &Config) -> anyhow::Result<Option<Arc<GatewayClient>>> {
  let push = &config.push;
  let gateway = &push.gateway;
  let Some(url) = gateway.maybe_url.clone().filter(|url| !url.is_empty()) else {
    anyhow::ensure!(
      gateway.maybe_key.is_none() && gateway.maybe_key_path.is_none(),
      "push.gateway: a key is set but no url"
    );
    return Ok(None);
  };
  require_tls(&url, "push.gateway.url", push.allow_private_endpoints)?;
  let key = match (&gateway.maybe_key, &gateway.maybe_key_path) {
    (Some(key), _) => parse_gateway_key(key.expose_secret()).context("push.gateway.key")?,
    (None, Some(path)) => {
      load_or_create_gateway_key(path).with_context(|| format!("push.gateway.key_path {}", path.display()))?
    },
    (None, None) => anyhow::bail!("push.gateway.key or key_path is required with push.gateway.url"),
  };
  let client = GatewayClient::new(GatewaySettings {
    url,
    key,
    timeout: Duration::from_secs(push.request_timeout_secs),
    accept_unbound_tokens: gateway.accept_unbound_tokens,
  })
  .context("push.gateway")?;
  info!(server_id = %client.server_id(), "push gateway enabled");
  let registering = client.clone();
  tokio::spawn(async move {
    if let Err(error) = registering.register().await {
      warn!(%error, "registering at the push gateway failed; the first push tries again");
    }
  });
  Ok(Some(client))
}

/// APNs is on when the key and its three identifiers are set; a partial setup is an error, not a
/// silently disabled provider.
fn apns_settings(config: &Config) -> anyhow::Result<Option<ApnsSettings>> {
  let push = &config.push;
  let apns = &push.apns;
  let maybe_key = secret_from_config(
    apns.maybe_key.as_ref(),
    apns.maybe_key_path.as_deref(),
    "push.apns.key",
  )?;
  let Some(key_pem) = maybe_key else {
    anyhow::ensure!(
      apns.maybe_key_id.is_none() && apns.maybe_team_id.is_none() && apns.maybe_bundle_id.is_none(),
      "push.apns: key_id, team_id or bundle_id is set but no key (push.apns.key or key_path)"
    );
    return Ok(None);
  };
  let required = |value: &Option<String>, name: &str| {
    value
      .clone()
      .filter(|value| !value.is_empty())
      .with_context(|| format!("push.apns.{name} is required with an APNs key"))
  };
  let base_url = apns.maybe_base_url.clone().unwrap_or_else(|| {
    if apns.use_sandbox {
      APNS_SANDBOX_URL
    } else {
      APNS_PRODUCTION_URL
    }
    .into()
  });
  require_tls(&base_url, "push.apns.base_url", push.allow_private_endpoints)?;
  Ok(Some(ApnsSettings {
    key_pem,
    key_id: required(&apns.maybe_key_id, "key_id")?,
    team_id: required(&apns.maybe_team_id, "team_id")?,
    bundle_id: required(&apns.maybe_bundle_id, "bundle_id")?,
    base_url,
    alert_text: apns.alert_text.clone(),
    timeout: Duration::from_secs(push.request_timeout_secs),
  }))
}

/// Push credentials (the APNs provider token, the FCM assertion and access token) only travel over
/// TLS. Plain `http://` is for local fakes and needs `push.allow_private_endpoints`.
fn require_tls(url: &str, name: &str, allow_plain_http: bool) -> anyhow::Result<()> {
  if url.starts_with("https://") {
    return Ok(());
  }
  anyhow::ensure!(
    allow_plain_http && url.starts_with("http://"),
    "{name} must be an https URL (plain http only with push.allow_private_endpoints, for development)"
  );
  warn!(
    setting = name,
    "push credentials are sent over plain http (development only)"
  );
  Ok(())
}

/// A secret given inline or as a file (for container secrets mounted as files). Inline wins.
fn secret_from_config(
  maybe_inline: Option<&SecretString>,
  maybe_path: Option<&std::path::Path>,
  name: &str,
) -> anyhow::Result<Option<SecretString>> {
  if let Some(inline) = maybe_inline {
    return Ok(Some(SecretString::from(inline.expose_secret().to_owned())));
  }
  let Some(path) = maybe_path else {
    return Ok(None);
  };
  let text =
    std::fs::read_to_string(path).with_context(|| format!("reading {name} from {}", path.display()))?;
  Ok(Some(SecretString::from(text)))
}

fn build_validator(config: &Config) -> anyhow::Result<TokenValidator> {
  let oidc = &config.oidc;
  let source = HttpJwkSource::new(
    oidc.issuer.clone(),
    oidc.maybe_jwks_uri.clone(),
    Duration::from_secs(oidc.http_timeout_secs),
  )
  .context("building the HTTP client for the identity provider")?;
  let keys = JwksCache::new(
    Box::new(source),
    JwksCacheOptions {
      ttl: Duration::from_secs(oidc.jwks_cache_ttl_secs),
      min_refresh_interval: Duration::from_secs(oidc.jwks_min_refresh_interval_secs),
    },
  );
  let algorithms = TokenValidator::parse_algorithms(&oidc.algorithms)
    .map_err(|e| anyhow::anyhow!("oidc.algorithms: {e}"))?;
  Ok(TokenValidator::new(
    keys,
    RoleMapping::new(oidc.roles.clone()),
    ValidatorOptions {
      issuer: oidc.issuer.clone(),
      client_id: oidc.client_id.clone(),
      algorithms,
      leeway_secs: oidc.leeway_secs,
      logout_audiences: vec![oidc.client_id.clone(), oidc.server_client.clone()],
      maybe_organization_claim: oidc.maybe_organization_claim.clone(),
    },
  ))
}

/// `--config <path>` or `TEAMCRAFT_CONFIG`; without either only environment variables are used.
fn config_path() -> Option<PathBuf> {
  let mut args = std::env::args().skip(1);
  while let Some(arg) = args.next() {
    if arg == "--config" {
      return args.next().map(PathBuf::from);
    }
    if let Some(path) = arg.strip_prefix("--config=") {
      return Some(PathBuf::from(path));
    }
  }
  std::env::var_os(CONFIG_PATH_ENV).map(PathBuf::from)
}

/// The push settings of the routing; a ring waits at most as long as it rings.
fn push_settings(config: &Config) -> PushSettings {
  let push = &config.push;
  let call_ttl_secs = match push.call_ttl_secs {
    0 => config.calls.as_ref().map_or(30, |calls| calls.ring_timeout_secs),
    secs => secs,
  };
  PushSettings {
    ttls: PushTtls {
      message: Duration::from_secs(push.ttl_secs),
      call: Duration::from_secs(call_ttl_secs),
      dismiss: Duration::from_secs(push.dismiss_ttl_secs),
      test: Duration::from_secs(push.test_ttl_secs),
    },
    retry: RetryPolicy {
      max_retries: push.max_retries,
      initial_delay: Duration::from_millis(push.retry_initial_delay_ms),
      max_delay: Duration::from_secs(push.retry_max_delay_secs),
      workers: push.retry_workers.max(1),
      max_queued_per_device: push.retry_max_queued_per_device,
      poll_interval: Duration::from_millis(push.retry_poll_interval_ms.max(10)),
    },
    test_interval: Duration::from_secs(push.test_interval_secs),
    max_rings_per_minute: push.max_rings_per_minute,
    dismiss_coalesce: Duration::from_millis(push.dismiss_coalesce_ms),
    allow_private_endpoints: push.allow_private_endpoints,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn push_credentials_need_tls_unless_development_allows_plain_http() {
    assert!(require_tls("https://api.push.apple.com", "x", false).is_ok());
    assert!(require_tls("http://127.0.0.1:9000", "x", false).is_err());
    assert!(require_tls("http://127.0.0.1:9000", "x", true).is_ok());
    assert!(require_tls("ftp://push.example.com", "x", true).is_err());
  }
}
