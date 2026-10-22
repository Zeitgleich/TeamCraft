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
use teamcraft_server::brand::BrandRegistry;
use teamcraft_server::config::{CONFIG_PATH_ENV, Config};
use teamcraft_server::gateway::{Hub, HubOptions};
use teamcraft_server::listener::bind_listener;
use teamcraft_server::logging;
use teamcraft_server::meetings::caldav::{CalDavStore, CalendarStore};
use teamcraft_server::meetings::mailer::{Mailer, SmtpMailer};
use teamcraft_server::meetings::{MeetingBackends, MeetingService};
use teamcraft_server::push::crypto::VapidKey;
use teamcraft_server::push::{
  APNS_PRODUCTION_URL, APNS_SANDBOX_URL, ApnsSender, ApnsSettings, FCM_BASE_URL, FcmSender, FcmSettings,
  GatewayClient, GatewaySettings, PushDispatcher, ServiceAccount, UnifiedPushSender, Vapid, WebPushSender,
  load_or_create_gateway_key, parse_gateway_key,
};
use teamcraft_server::rendezvous::RendezvousHub;
use teamcraft_server::router::router;
use teamcraft_server::security::RateLimits;
use teamcraft_server::services::calls::CallService;
use teamcraft_server::services::local_accounts::LocalAuth;
use teamcraft_server::services::permission_scheme::{self, SchemeStore};
use teamcraft_server::services::presence::{self, refresh_presence};
use teamcraft_server::services::session::{self, PolicyHolder};
use teamcraft_server::services::{backgrounds, email_digest, guests, oidc, retention, urgent};
use teamcraft_server::state::{AppState, OidcSyncSettings, PushSettings, Settings};
use teamcraft_server::storage::BlobStore;
use tracing::{info, warn};

const SESSION_SWEEP_INTERVAL: Duration = Duration::from_secs(30);
const BACKGROUND_SWEEP_INTERVAL: Duration = Duration::from_secs(600);

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
  let brands = BrandRegistry::build(&config.branding, &config.brands, &config.web, &config.mail)
    .context("checking [branding] and [[brands]]")?;
  logging::init(&config.logging)?;

  let database = Database::connect(
    config.database.url.expose_secret(),
    &DatabaseOptions {
      max_connections: config.database.max_connections,
      acquire_timeout: config.database.acquire_timeout(),
    },
  )
  .await
  .context("connecting to the database")?;
  database.migrate().await.context("running migrations")?;
  database
    .ensure_partitions(config.server.partitions_ahead_months)
    .await
    .context("creating message partitions")?;

  let state = AppState {
    database,
    validator: Arc::new(build_validator(&config)?),
    ids: Arc::new(SnowflakeGenerator::new(config.server.worker_id).context("invalid server.worker_id")?),
    push: Arc::new(build_push(&config)?),
    revocations: Arc::new(Revocations::default()),
    hub: Arc::new(Hub::new(HubOptions {
      resume_window: Duration::from_secs(config.gateway.resume_window_secs),
      buffer_events: config.gateway.resume_buffer_events,
      max_sessions_per_device: config.gateway.max_sessions_per_device,
      max_sessions_per_user: config.gateway.max_sessions_per_user,
    })),
    rendezvous: Arc::new(RendezvousHub::new(&config.rendezvous)),
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
    maybe_meetings: build_meetings(&config)?.map(Arc::new),
    maybe_local: build_local_auth(&config)?.map(Arc::new),
    maybe_mailer: match &config.mail.smtp {
      Some(smtp) => {
        Some(Arc::new(SmtpMailer::new(smtp).context("configuring the mail server")?) as Arc<dyn Mailer>)
      },
      None => None,
    },
    policy: Arc::new(PolicyHolder::default()),
    scheme: Arc::new(SchemeStore::default()),
    settings: Arc::new(Settings {
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
      presence: config.presence.clone(),
      shares: config.shares.clone(),
      maybe_organization_claim: config.oidc.maybe_organization_claim.clone(),
      branding: config.branding.clone(),
      brands: config.brands.clone(),
      push: PushSettings {
        ttl: Duration::from_secs(config.push.ttl_secs),
        allow_private_endpoints: config.push.allow_private_endpoints,
      },
    }),
    rate_limits: Arc::new(RateLimits::new(&config.security.rate_limits)),
    brands: Arc::new(brands),
  };
  spawn_session_sweeper(state.clone());
  if state.settings.maybe_oidc_sync.is_some() {
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
  spawn_derived_presence(state.clone());

  let listener = bind_listener(config.server.listen_addr)
    .with_context(|| format!("binding {}", config.server.listen_addr))?;
  info!(addr = %config.server.listen_addr, "teamcraft-server listening");
  axum::serve(
    listener,
    router(state).into_make_service_with_connect_info::<SocketAddr>(),
  )
  .with_graceful_shutdown(shutdown_signal())
  .await?;
  info!("shut down");
  Ok(())
}

/// Removes deleted and abandoned call backgrounds once their upload URLs have expired.
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
    let settings = state.settings.presence.clone();
    let mut interval = tokio::time::interval(Duration::from_secs(settings.auto_status_interval_secs.max(1)));
    let poll_every = Duration::from_secs(settings.calendar_poll_interval_secs.max(1));
    let mut maybe_last_poll: Option<tokio::time::Instant> = None;
    loop {
      interval.tick().await;
      let is_poll_due = maybe_last_poll.is_none_or(|at| at.elapsed() >= poll_every);
      if is_poll_due {
        maybe_last_poll = Some(tokio::time::Instant::now());
      }
      if let Some(minutes) = state.policy.get().presence.maybe_away_timeout_minutes {
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
    let settings = state.settings.retention.clone();
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
  if let Some(storage) = &config.storage {
    urls.push(&storage.endpoint);
  }
  let urls: Vec<String> = urls.into_iter().map(str::to_owned).collect();
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

/// The meeting feature, if `[meetings]` is configured.
fn build_meetings(config: &Config) -> anyhow::Result<Option<MeetingService>> {
  let Some(meetings) = &config.meetings else {
    return Ok(None);
  };
  let maybe_calendar = match &meetings.caldav {
    Some(caldav) => {
      let store = CalDavStore::new(caldav).context("configuring the CalDAV client")?;
      Some(Arc::new(store) as Arc<dyn CalendarStore>)
    },
    None => None,
  };
  let maybe_mailer = match &meetings.smtp {
    Some(smtp) => {
      let mailer = SmtpMailer::new(smtp).context("configuring the mail server")?;
      Some(Arc::new(mailer) as Arc<dyn Mailer>)
    },
    None => None,
  };
  MeetingService::new(
    meetings,
    MeetingBackends {
      maybe_calendar,
      maybe_mailer,
    },
  )
  .map(Some)
  .context("configuring meetings")
}

/// Removes guests whose membership expired (and tells their sponsors) and forgets old sessions.
fn spawn_account_housekeeping(state: AppState) {
  tokio::spawn(async move {
    let period = Duration::from_secs(state.settings.guests.expiry_check_interval_secs.max(1));
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
    }
  });
}

/// Pushes urgent direct messages again until they are acknowledged (PLAN.md §5).
fn spawn_urgent_scheduler(state: AppState) {
  tokio::spawn(async move {
    let period = Duration::from_secs(state.settings.notifications.urgent_scheduler_interval_secs.max(1));
    let mut interval = tokio::time::interval(period);
    loop {
      interval.tick().await;
      if let Err(error) = urgent::run_due(&state, time::OffsetDateTime::now_utc()).await {
        warn!(%error, "urgent reminders failed");
      }
    }
  });
}

/// Mails the digests of missed mentions that are due (PLAN.md §5).
fn spawn_email_digests(state: AppState) {
  if !state.settings.notifications.email_digest.is_enabled {
    return;
  }
  tokio::spawn(async move {
    let period = Duration::from_secs(
      state
        .settings
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
    dispatcher = dispatcher.with_token_sender(&PushProvider::Apns, sender);
  } else if let Some(gateway) = &maybe_gateway {
    dispatcher = dispatcher.with_token_sender(&PushProvider::Apns, gateway.sender(PushProvider::Apns));
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
    dispatcher = dispatcher.with_token_sender(&PushProvider::Fcm, sender);
  } else if let Some(gateway) = &maybe_gateway {
    dispatcher = dispatcher.with_token_sender(&PushProvider::Fcm, gateway.sender(PushProvider::Fcm));
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
