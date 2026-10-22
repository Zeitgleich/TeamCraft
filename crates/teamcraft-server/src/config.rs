use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use figment::Figment;
use figment::providers::{Env, Format, Toml};
use secrecy::SecretString;
use serde::Deserialize;

/// Prefix of environment overrides. Nested keys are separated by a double underscore:
/// `TEAMCRAFT_OIDC__ISSUER`, `TEAMCRAFT_OIDC__ROLES__GUEST`, `TEAMCRAFT_DATABASE__URL`.
const ENV_PREFIX: &str = "TEAMCRAFT_";
const ENV_NESTING: &str = "__";
/// Environment variable naming the config file itself; not a config key.
pub const CONFIG_PATH_ENV: &str = "TEAMCRAFT_CONFIG";

/// Complete server configuration. `teamcraft.example.toml` documents every option.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
  #[serde(default)]
  pub server: ServerConfig,
  pub database: DatabaseConfig,
  pub oidc: OidcConfig,
  #[serde(default)]
  pub logging: LoggingConfig,
  #[serde(default)]
  pub policy: PolicyConfig,
  #[serde(default)]
  pub limits: LimitsConfig,
  #[serde(default)]
  pub gateway: GatewayConfig,
  #[serde(default)]
  pub rendezvous: RendezvousConfig,
  #[serde(default)]
  pub retention: RetentionJobConfig,
  #[serde(default)]
  pub push: PushConfig,
  #[serde(default)]
  pub notifications: NotificationsConfig,
  #[serde(default)]
  pub presence: PresenceConfig,
  #[serde(default)]
  pub shares: SharesConfig,
  #[serde(default)]
  pub integration: IntegrationConfig,
  #[serde(default)]
  pub web: WebConfig,
  #[serde(default)]
  pub security: SecurityConfig,
  #[serde(default)]
  pub guests: GuestsConfig,
  #[serde(default)]
  pub mail: MailConfig,
  #[serde(default)]
  pub local_accounts: LocalAccountsConfig,
  /// Blob storage for attachments; without it the attachment endpoints answer 503.
  #[serde(default)]
  pub storage: Option<StorageConfig>,
  /// Calls through the SFU; without it the join-token endpoint answers 503.
  #[serde(default)]
  pub calls: Option<CallsConfig>,
  /// Meetings (PLAN.md §9); without it the meeting endpoints answer 503.
  #[serde(default)]
  pub meetings: Option<MeetingsConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerConfig {
  /// Address of the HTTP listener. TLS is terminated by the reverse proxy. The default `[::]` is a
  /// dual-stack socket (IPv4 and IPv6); set e.g. `127.0.0.1:8080` to bind a single address.
  pub listen_addr: SocketAddr,
  /// Unique number (0-1023) of this server instance in generated Snowflake IDs.
  pub worker_id: u16,
  /// How many months of message partitions are created ahead of time.
  pub partitions_ahead_months: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
  /// `postgres://user:password@host:5432/database`
  pub url: SecretString,
  #[serde(default = "default_max_connections")]
  pub max_connections: u32,
  #[serde(default = "default_acquire_timeout_secs")]
  pub acquire_timeout_secs: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OidcConfig {
  /// Issuer URL; must equal the `iss` claim of the tokens and the issuer in the discovery document.
  pub issuer: String,
  /// Public client the apps log in with. Tokens must be issued to it (`azp` or `aud`), and its
  /// client roles are read from `resource_access.<client_id>.roles`.
  #[serde(default = "default_client_id")]
  pub client_id: String,
  /// Confidential client with a service account, reserved for the admin API and back-channel logout.
  #[serde(default = "default_server_client")]
  pub server_client: String,
  /// Overrides the `jwks_uri` of the discovery document (e.g. an internal URL, or no discovery).
  #[serde(rename = "jwks_uri", default)]
  pub maybe_jwks_uri: Option<String>,
  /// Accepted JWS algorithms, e.g. `RS256`, `ES256`, `EdDSA`.
  #[serde(default = "default_algorithms")]
  pub algorithms: Vec<String>,
  /// Tolerated clock difference when checking `exp` and `nbf`.
  #[serde(default = "default_leeway_secs")]
  pub leeway_secs: u64,
  /// How long fetched signing keys are used before they are refreshed.
  #[serde(default = "default_jwks_cache_ttl_secs")]
  pub jwks_cache_ttl_secs: u64,
  /// Minimum time between two key fetches, also when a token names an unknown key ID.
  #[serde(default = "default_jwks_min_refresh_interval_secs")]
  pub jwks_min_refresh_interval_secs: u64,
  /// Timeout of requests to the identity provider.
  #[serde(default = "default_http_timeout_secs")]
  pub http_timeout_secs: u64,
  #[serde(default)]
  pub roles: RoleNames,
  /// Periodic comparison with the provider's admin API (disabled users, role changes).
  #[serde(default)]
  pub sync: OidcSyncConfig,
}

/// Deprovisioning by polling the identity provider's admin API with the service account of
/// `server_client` (ARCHITECTURE.md §5.1.1). Back-channel logout covers sessions; this covers
/// disabled users and role changes.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OidcSyncConfig {
  pub is_enabled: bool,
  pub interval_secs: u64,
  pub initial_delay_secs: u64,
  /// Secret of `server_client`.
  #[serde(rename = "client_secret")]
  pub maybe_client_secret: Option<SecretString>,
  /// Keycloak admin API of the realm, e.g. `https://auth.example.com/admin/realms/company`.
  /// Derived from the issuer if the issuer looks like `<base>/realms/<realm>`.
  #[serde(rename = "admin_url")]
  pub maybe_admin_url: Option<String>,
  /// Users fetched per request.
  pub page_size: u32,
  /// Mirror the provider's top-level groups as user groups (`@handle`), members included. The
  /// handle is made from the group name; groups whose name cannot become a handle are skipped.
  pub sync_groups: bool,
}

/// Names of the identity provider's client roles per TeamCraft role (ARCHITECTURE.md §5.1.1).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RoleNames {
  pub owner: String,
  pub admin: String,
  pub member: String,
  pub guest: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LoggingConfig {
  /// `tracing` filter directive, e.g. `info` or `teamcraft_server=debug,info`. `RUST_LOG` wins.
  pub filter: String,
  pub format: LogFormat,
}

/// Instance policies with defaults (PLAN.md §12, §13). Later they move into signed, runtime-editable policy.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PolicyConfig {
  /// Guests can only open DMs with people they share a conversation with.
  pub guests_dm_only_with_shared_conversations: bool,
  /// Pending invites only apply to accounts whose identity provider verified the email address.
  pub invites_require_verified_email: bool,
}

/// Size limits of requests and results.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LimitsConfig {
  pub max_ciphertext_bytes: usize,
  pub max_mentions_per_message: usize,
  pub max_group_dm_participants: usize,
  pub max_name_length: usize,
  pub max_text_length: usize,
  pub default_page_size: u32,
  pub max_page_size: u32,
  pub max_directory_results: u32,
  /// Largest avatar image.
  pub max_avatar_bytes: u64,
  /// Largest call background picture a user uploads (encrypted; apps send Full HD JPEGs).
  pub max_background_bytes: u64,
  /// How many call background pictures one user may keep.
  pub max_backgrounds_per_user: u32,
  /// How long a request for organization recovery waits for approvals.
  pub org_recovery_request_ttl_secs: u64,
  /// Largest sealed share or sealed backup key.
  pub max_recovery_blob_bytes: usize,
  /// Longest title, department and similar profile text, in characters.
  pub max_profile_text_chars: usize,
  /// Members of a user group.
  pub max_user_group_members: usize,
  /// Unread counts look at this many of the newest unread messages per conversation; a count equal
  /// to it means "that many or more" (clients show "1000+").
  pub max_unread_counted: u32,
  /// Unused key packages a device may have on the server.
  pub max_key_packages_per_device: usize,
  pub max_key_package_bytes: usize,
  /// Key packages in one upload.
  pub max_key_packages_per_upload: usize,
  /// Size of one MLS commit, welcome, proposal or application message.
  pub max_mls_payload_bytes: usize,
  pub max_welcomes_per_commit: usize,
  /// Encrypted key bag plus wrapped backup keys of one user.
  pub max_key_bag_bytes: usize,
  /// One signed device list.
  pub max_device_list_bytes: usize,
  /// Active devices one user may have; registering more fails until one is revoked.
  pub max_devices_per_user: usize,
  /// Attachments one user may have announced but not finished uploading.
  pub max_pending_attachments_per_user: usize,
  /// Bytes of attachments one user may have stored or announced in total.
  pub max_attachment_bytes_per_user: u64,
  /// Meeting e-mails (invitations, updates, cancellations) one organizer's meetings may send per
  /// hour; the rest of the invitees are told in the app only.
  pub max_meeting_mails_per_user_per_hour: u32,
}

/// WebSocket gateway behaviour.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GatewayConfig {
  /// Interval the client must heartbeat at.
  pub heartbeat_interval_secs: u64,
  /// A connection without any frame for this long is closed.
  pub heartbeat_timeout_secs: u64,
  /// How long a disconnected session can be resumed.
  pub resume_window_secs: u64,
  /// Events kept per session for resuming.
  pub resume_buffer_events: usize,
  /// Time a new connection has to identify.
  pub identify_timeout_secs: u64,
  /// Connections are closed after this long so that a fresh token is checked; clients resume.
  pub connection_max_age_secs: u64,
  /// A device that disconnected only counts as offline after this grace period (reconnects are common).
  pub presence_offline_grace_secs: u64,
  /// Largest accepted client frame.
  pub max_frame_bytes: usize,
  /// Frames one connection may send per minute on average (heartbeats and typing included);
  /// a connection above it is closed.
  pub max_client_frames_per_minute: u32,
  /// Frames a connection may send in a burst before the average applies.
  pub client_frame_burst: u32,
  /// Sessions (live or waiting to be resumed) one device may have; the oldest are dropped.
  pub max_sessions_per_device: usize,
  /// Sessions one user may have in total.
  pub max_sessions_per_user: usize,
}

/// S3-compatible blob storage (Garage in development). Everything stored there is encrypted by the
/// clients; the provider is untrusted.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
  /// Endpoint URL clients can reach (they upload and download through presigned URLs).
  pub endpoint: String,
  #[serde(default = "default_region")]
  pub region: String,
  pub bucket: String,
  pub access_key: String,
  pub secret_key: SecretString,
  /// Path-style addressing (`endpoint/bucket/key`), needed by Garage and MinIO.
  #[serde(default = "default_true")]
  pub path_style: bool,
  /// Validity of presigned URLs.
  #[serde(default = "default_presign_expiry_secs")]
  pub presign_expiry_secs: u64,
  /// Largest attachment (ciphertext) a client may announce.
  #[serde(default = "default_max_attachment_bytes")]
  pub max_attachment_bytes: u64,
}

/// Push sending (e2ee.md §7). Web Push needs a VAPID key pair; APNs, FCM and UnifiedPush are not
/// implemented yet.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PushConfig {
  /// The VAPID private key: 32 bytes, base64url. Without it Web Push devices get nothing.
  #[serde(rename = "vapid_private_key")]
  pub maybe_vapid_private_key: Option<SecretString>,
  /// Contact for push services, `mailto:` or `https:` (RFC 8292).
  pub vapid_subject: String,
  /// How long push services may hold a wake-up for an offline device.
  pub ttl_secs: u64,
  pub request_timeout_secs: u64,
  /// Pushes sent in parallel.
  pub max_concurrent_sends: usize,
  /// Accept Web Push endpoints on private addresses and plain http. For development only.
  pub allow_private_endpoints: bool,
}

/// The background job that enforces retention (deletes old ciphertext, attachments and index segments).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RetentionJobConfig {
  pub is_enabled: bool,
  /// Time between runs.
  pub interval_secs: u64,
  /// Delay before the first run after startup.
  pub initial_delay_secs: u64,
  /// Months of message partitions kept ready ahead of time (the job tops them up).
  pub partitions_ahead_months: u32,
}

/// Guests: external people with a sponsor and an expiry (PLAN.md §12).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GuestsConfig {
  /// Guests added or invited without an explicit expiry expire this many days later; `0` means
  /// they never do.
  pub default_expiry_days: u32,
  /// If not empty, guests can only be invited with e-mail addresses of these domains (and their
  /// subdomains); lower case, without `@`.
  pub allowed_email_domains: Vec<String>,
  /// How often expired guests are removed.
  pub expiry_check_interval_secs: u64,
}

impl Default for GuestsConfig {
  fn default() -> Self {
    Self {
      default_expiry_days: 90,
      allowed_email_domains: Vec::new(),
      expiry_check_interval_secs: 3600,
    }
  }
}

/// Outgoing e-mail of the server itself: invitations to spaces and channels. (Meeting invitations
/// have their own settings under `[meetings]`.)
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MailConfig {
  /// The mail server; without it no e-mail is sent and invitations are only applied at sign-in.
  pub smtp: Option<SmtpConfig>,
  /// Public base URL of the web client, without trailing slash; the link in invitation e-mail.
  pub public_base_url: String,
  /// Placeholders of both texts: `{inviter}`, `{space}`, `{role}` and `{url}`.
  pub invite_subject: String,
  pub invite_body: String,
  /// Activation e-mail of local guest accounts. Placeholders: `{inviter}`, `{space}` and `{url}`
  /// (the link with the one-time token).
  pub activation_subject: String,
  pub activation_body: String,
}

impl Default for MailConfig {
  fn default() -> Self {
    Self {
      smtp: None,
      public_base_url: String::new(),
      invite_subject: "{inviter} invited you to {space}".into(),
      invite_body:
        "{inviter} invited you to \"{space}\" as {role}.\n\nSign in with this e-mail address to join: {url}\n"
          .into(),
      activation_subject: "{inviter} invited you to {space}".into(),
      activation_body:
        "{inviter} invited you to \"{space}\".\n\nSet your password and your authenticator app here: {url}\n"
          .into(),
    }
  }
}

/// Guests with a password and TOTP of their own, for people who are not in the identity provider
/// (PLAN.md §12).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LocalAccountsConfig {
  /// Off by default: without it the endpoints answer 503 and local tokens are not accepted.
  pub is_enabled: bool,
  /// Signs session tokens and seals stored TOTP secrets (at least 32 bytes of randomness).
  /// Prefer `TEAMCRAFT_LOCAL_ACCOUNTS__SESSION_SECRET`.
  #[serde(rename = "session_secret")]
  pub maybe_session_secret: Option<SecretString>,
  /// The activation link is valid this long.
  pub activation_ttl_hours: u32,
  /// A session ends after this long, whatever the workspace session policy says (it can only
  /// shorten it).
  pub session_ttl_hours: u32,
  pub min_password_chars: usize,
  /// Every account needs a TOTP code, not only those that confirmed one.
  pub require_totp: bool,
  /// Failed sign-ins in a row that lock the account.
  pub max_failed_attempts: u32,
  pub lockout_secs: u64,
  /// Shown by authenticator apps.
  pub totp_issuer: String,
  /// Argon2id cost of password hashes.
  pub password_memory_kib: u32,
  pub password_iterations: u32,
  pub password_parallelism: u32,
}

impl Default for LocalAccountsConfig {
  fn default() -> Self {
    Self {
      is_enabled: false,
      maybe_session_secret: None,
      activation_ttl_hours: 72,
      session_ttl_hours: 12,
      min_password_chars: 12,
      require_totp: true,
      max_failed_attempts: 5,
      lockout_secs: 900,
      totp_issuer: "TeamCraft".into(),
      password_memory_kib: 19 * 1024,
      password_iterations: 2,
      password_parallelism: 1,
    }
  }
}

/// Request limits and response hardening of the HTTP API.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SecurityConfig {
  /// Largest request body; bigger ones get 413. Ciphertext, key packages and MLS payloads have
  /// their own, smaller limits (`[limits]`); this is the ceiling in front of them.
  pub max_request_body_bytes: usize,
  /// Send `Strict-Transport-Security`. Enable it when a TLS-terminating proxy serves the API.
  pub hsts: bool,
  /// The server runs behind a reverse proxy that sets `X-Forwarded-For`: use the last address in
  /// it as the client address (the one the proxy saw). Leave off when clients connect directly,
  /// since anybody could forge the header.
  pub trust_forwarded_for: bool,
  pub rate_limits: RateLimitConfig,
}

/// Per-minute limits; each is a token bucket with a burst of the same size. Counted in memory per
/// server instance.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RateLimitConfig {
  pub is_enabled: bool,
  /// All requests of one client address.
  pub ip_requests_per_minute: u32,
  /// Writes (POST, PUT, PATCH, DELETE) of one authenticated user.
  pub user_writes_per_minute: u32,
  /// Requests without credentials: guest join, back-channel logout, internal endpoints.
  pub public_requests_per_minute: u32,
  /// Failed authentications (401) of one client address; once used up, that address gets 429 until
  /// the bucket refills, which slows token guessing.
  pub auth_failures_per_minute: u32,
}

impl Default for SecurityConfig {
  fn default() -> Self {
    Self {
      max_request_body_bytes: 4 * 1024 * 1024,
      hsts: false,
      trust_forwarded_for: false,
      rate_limits: RateLimitConfig::default(),
    }
  }
}

impl Default for RateLimitConfig {
  fn default() -> Self {
    Self {
      is_enabled: true,
      ip_requests_per_minute: 1200,
      user_writes_per_minute: 600,
      public_requests_per_minute: 30,
      auth_failures_per_minute: 30,
    }
  }
}

/// Serving the web client (the `trunk build` output of `teamcraft-ui`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WebConfig {
  /// Directory with `index.html` and the hashed assets; without it the server only serves the API.
  #[serde(rename = "static_dir")]
  pub maybe_static_dir: Option<PathBuf>,
  /// `Content-Security-Policy` of every page and asset. The default allows the app's own origin,
  /// WebAssembly and `blob:`/`data:` images and media. `{connect_src}` stands for the origins the
  /// app may talk to: its own, the identity provider's, the SFU's, the storage's and
  /// `connect_origins`; a policy without it is used as written.
  pub content_security_policy: String,
  /// Further origins (`https://...`, `wss://...`) the web client may connect to.
  pub connect_origins: Vec<String>,
  /// Directory with the small static download page for share links: `download.html` and the
  /// assets it references. `{{sri:<file>}}` in the page becomes the Subresource Integrity value
  /// (`sha384-...`) of that file in this directory, computed at startup. Without it `/f/<id>`
  /// serves the app shell.
  #[serde(rename = "share_page_dir")]
  pub maybe_share_page_dir: Option<PathBuf>,
  /// `Content-Security-Policy` of the download page: no inline script, nothing but its own files.
  pub share_page_content_security_policy: String,
}

impl Default for WebConfig {
  fn default() -> Self {
    Self {
      maybe_static_dir: None,
      maybe_share_page_dir: None,
      connect_origins: Vec::new(),
      share_page_content_security_policy:
        "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; \
        img-src 'self' data:; connect-src 'self' https:; base-uri 'none'; form-action 'none'; \
        frame-ancestors 'none'"
          .into(),
      content_security_policy: "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; \
        img-src 'self' data: blob:; media-src 'self' blob:; font-src 'self'; connect-src {connect_src}; \
        worker-src 'self' blob:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"
        .into(),
    }
  }
}

impl WebConfig {
  /// Adds the origins of the identity provider, the SFU and the storage to the ones the web client
  /// may connect to. Unparseable URLs are skipped here and reported by their own sections.
  pub fn allow_connections_to(&mut self, urls: &[&str]) {
    for url in urls {
      if let Ok(parsed) = url::Url::parse(url) {
        let origin = parsed.origin().ascii_serialization();
        if origin != "null" && !self.connect_origins.contains(&origin) {
          self.connect_origins.push(origin);
        }
      }
    }
  }

  /// The policy as it is sent: `{connect_src}` replaced by `'self'` and the allowed origins.
  pub fn resolved_content_security_policy(&self) -> String {
    let mut sources = vec!["'self'".to_owned()];
    sources.extend(self.connect_origins.iter().cloned());
    self
      .content_security_policy
      .replace("{connect_src}", &sources.join(" "))
  }

  /// Rejects a configuration that would send no policy or a broken one.
  pub fn validate(&self) -> Result<(), String> {
    for (name, policy) in [
      ("content_security_policy", self.resolved_content_security_policy()),
      (
        "share_page_content_security_policy",
        self.share_page_content_security_policy.clone(),
      ),
    ] {
      if policy.trim().is_empty() || axum::http::HeaderValue::from_str(&policy).is_err() {
        return Err(format!("web.{name} is empty or not a valid header value"));
      }
    }
    for origin in &self.connect_origins {
      let is_plain = origin
        .bytes()
        .all(|b| b.is_ascii_graphic() && !matches!(b, b';' | b','));
      let has_scheme = ["https://", "wss://", "http://", "ws://"]
        .iter()
        .any(|s| origin.starts_with(s));
      if !is_plain || !has_scheme {
        return Err(format!("web.connect_origins: {origin:?} is not an origin"));
      }
    }
    Ok(())
  }
}

/// Defaults of integration channels (ARCHITECTURE.md §4.5).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct IntegrationConfig {
  /// Retention of a channel that becomes an integration channel, in days; `0` sets none. A
  /// channel's own setting wins.
  pub default_retention_days: u32,
}

impl Default for IntegrationConfig {
  fn default() -> Self {
    Self {
      default_retention_days: 30,
    }
  }
}

/// Meetings (PLAN.md §9, ARCHITECTURE.md §6.4).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeetingsConfig {
  /// Public base URL of this server, without trailing slash. Join links are `<base>/meeting/<id>`
  /// and the host part names the iCalendar UIDs.
  pub public_base_url: String,
  /// Address used as organizer of iCalendar events when the organizer has no e-mail address.
  pub organizer_fallback_address: String,
  /// The calendar server events are written to; without it meetings are not stored in a calendar.
  #[serde(default)]
  pub caldav: Option<CalDavConfig>,
  /// Most invitees of one meeting.
  #[serde(default = "default_max_meeting_invitees")]
  pub max_invitees: usize,
  /// Key that signs the personal join links and guest sessions (at least 32 bytes of randomness).
  /// Prefer `TEAMCRAFT_MEETINGS__GUEST_TOKEN_SECRET`.
  pub guest_token_secret: SecretString,
  /// Guests may use their link until this long after the meeting's end.
  #[serde(default = "default_guest_grace_secs")]
  pub guest_grace_secs: u64,
  /// Shared secret of the inbox handler that forwards received RSVP e-mail (iMIP REPLY) to
  /// `POST /api/internal/v1/meetings/replies` as `Authorization: Bearer <token>`. Without it that
  /// endpoint answers 503. Prefer `TEAMCRAFT_MEETINGS__INBOX_TOKEN`.
  #[serde(rename = "inbox_token", default)]
  pub maybe_inbox_token: Option<SecretString>,
  /// Outgoing mail server for invitations (iMIP); without it no e-mail is sent.
  #[serde(default)]
  pub smtp: Option<SmtpConfig>,
  /// Subjects and texts of the invitation e-mails.
  #[serde(default)]
  pub mail_texts: MailTexts,
}

/// How the connection to the mail server is secured.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmtpSecurity {
  /// Plain connection upgraded with STARTTLS (port 587); the upgrade is required.
  #[default]
  StartTls,
  /// TLS from the first byte (port 465).
  Tls,
  /// No encryption; only for a local mail catcher in development.
  None,
}

/// SMTP server for invitation e-mails.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmtpConfig {
  pub host: String,
  #[serde(default = "default_smtp_port")]
  pub port: u16,
  #[serde(default)]
  pub security: SmtpSecurity,
  #[serde(default)]
  pub username: Option<String>,
  /// Prefer `TEAMCRAFT_MEETINGS__SMTP__PASSWORD`.
  #[serde(default)]
  pub password: Option<SecretString>,
  /// Sender address of invitations.
  pub from_address: String,
  #[serde(default)]
  pub from_name: Option<String>,
  #[serde(default = "default_smtp_timeout_secs")]
  pub timeout_secs: u64,
}

/// Texts of the invitation e-mails. Placeholders: `{title}`, `{organizer}`, `{start}`, `{end}`
/// (UTC), `{description}` and `{join_url}`. Admins translate them here.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MailTexts {
  pub invitation_subject: String,
  pub invitation_body: String,
  pub update_subject: String,
  pub update_body: String,
  pub cancellation_subject: String,
  pub cancellation_body: String,
}

/// A calendar collection on a CalDAV server and the account that writes to it. Events of all
/// meetings go into this one calendar (per-organizer calendars need per-user credentials, which the
/// server does not hold).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalDavConfig {
  /// URL of the calendar collection, e.g. `https://cal.example.com/dav/calendars/teamcraft/meetings/`.
  pub calendar_url: String,
  pub username: String,
  /// Password or app password of the account. Prefer `TEAMCRAFT_MEETINGS__CALDAV__PASSWORD`.
  pub password: SecretString,
  #[serde(default = "default_caldav_timeout_secs")]
  pub request_timeout_secs: u64,
  /// Where one user's calendar lives, with `{email}` for their e-mail address, e.g.
  /// `https://cal.example.com/dav/calendars/{email}/personal/`. When set, the server asks it
  /// for the busy periods of people who are online (a CalDAV free-busy report) and shows them as
  /// "in a meeting". The account above needs read access to those calendars.
  #[serde(rename = "free_busy_url_template", default)]
  pub maybe_free_busy_url_template: Option<String>,
}

/// Notification defaults and the persistent-notification scheduler for urgent direct messages
/// (PLAN.md §5).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct NotificationsConfig {
  /// Whether users who never chose send read receipts in DMs and group DMs.
  pub read_receipts_default: bool,
  /// Time between repeated pushes for an urgent DM nobody acknowledged.
  pub urgent_repeat_interval_secs: u64,
  /// How often an urgent DM is pushed in total, the first push included. `1` disables repeats.
  pub urgent_max_pushes: u32,
  /// How often the scheduler looks for due repeats.
  pub urgent_scheduler_interval_secs: u64,
  /// While a desktop device of the user is active, pushes to their phones wait this long and are
  /// sent only if the message is still unread. `0` pushes at once.
  pub mobile_push_delay_secs: u64,
}

/// Share links for external download (e2ee.md §4.5.1).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SharesConfig {
  /// Off: no links can be created and the public endpoints answer "not available". Who may create
  /// links is the `create_share_link` permission (members yes, guests no by default).
  pub is_enabled: bool,
  /// Links made without an expiry last this long.
  pub default_expiry_days: u32,
  /// No link lasts longer than this.
  pub max_expiry_days: u32,
  /// Most downloads a link may allow; `0` means no cap. Links made without a limit get the cap.
  pub max_downloads_cap: u32,
  /// Public URL of this server without trailing slash; links are `<base>/f/<id>`.
  pub public_base_url: String,
  /// Most live (not revoked, not expired) links per person.
  pub max_links_per_user: usize,
}

impl Default for SharesConfig {
  fn default() -> Self {
    Self {
      is_enabled: true,
      default_expiry_days: 7,
      max_expiry_days: 30,
      max_downloads_cap: 0,
      public_base_url: String::new(),
      max_links_per_user: 500,
    }
  }
}

/// Derived presence: "in a meeting", "in a call" and the away timeout (PLAN.md §4).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PresenceConfig {
  /// How often derived statuses and the away timeout are re-evaluated for online users.
  pub auto_status_interval_secs: u64,
  /// How often the calendar server is asked when online users are busy (needs
  /// `meetings.caldav.free_busy_url_template`).
  pub calendar_poll_interval_secs: u64,
  /// Busy periods starting within this time are fetched, so the status can switch on at their start.
  pub calendar_lookahead_secs: u64,
}

impl Default for PresenceConfig {
  fn default() -> Self {
    Self {
      auto_status_interval_secs: 30,
      calendar_poll_interval_secs: 300,
      calendar_lookahead_secs: 900,
    }
  }
}

/// Calls (ARCHITECTURE.md §6): the server issues join tokens the SFU verifies with its public key.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallsConfig {
  /// WebTransport URL of the SFU that clients connect to.
  pub sfu_url: String,
  /// `iss` of join tokens, usually the server's public base URL; the SFU expects the same value.
  pub issuer: String,
  /// PEM file with the Ed25519 private key that signs join tokens (`openssl genpkey -algorithm
  /// ed25519`). The SFU is configured with the matching public key.
  pub signing_key_path: PathBuf,
  /// Lifetime of a join token; it is only checked when joining, so keep it short.
  #[serde(default = "default_join_token_ttl_secs")]
  pub join_token_ttl_secs: u64,
  /// A conversation's call gets a new id once no join token was issued for this long.
  #[serde(default = "default_call_idle_secs")]
  pub call_idle_secs: u64,
  /// Shared secret of the SFU: it reports who joins and leaves calls with `Authorization: Bearer
  /// <token>` to `/api/internal/v1/calls/participants/*`. Without it those endpoints answer 503 and
  /// the call roster stays empty. Prefer `TEAMCRAFT_CALLS__SFU_REGISTRATION_TOKEN`.
  #[serde(rename = "sfu_registration_token", default)]
  pub maybe_sfu_registration_token: Option<SecretString>,
  /// DMs and group DMs ring for this long unless somebody else joins.
  #[serde(default = "default_ring_timeout_secs")]
  pub ring_timeout_secs: u64,
  /// Where the SFU takes host controls (mute all, remove, lock, end) on its internal listener, e.g.
  /// `http://sfu.internal:8081`. The registration token above authenticates the server. Without
  /// it the meeting controls answer 503.
  #[serde(rename = "sfu_control_url", default)]
  pub maybe_sfu_control_url: Option<String>,
}

/// Device-linking relay at /api/v1/rendezvous.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RendezvousConfig {
  /// Lifetime of a relay from the first join.
  pub ttl_secs: u64,
  /// Largest relayed binary frame.
  pub max_frame_bytes: usize,
  /// Total bytes relayed per rendezvous, both directions.
  pub max_total_bytes: usize,
  /// Frames held for a device whose peer has not joined yet.
  pub max_pending_frames: usize,
  /// Open relays per user.
  pub max_per_user: usize,
  /// Time a new connection has to join.
  pub join_timeout_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
  Pretty,
  Json,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
  #[error("invalid configuration: {0}")]
  Invalid(#[from] Box<figment::Error>),
  #[error("config file {0} does not exist")]
  MissingFile(String),
}

impl Config {
  /// Loads the optional TOML file and applies `TEAMCRAFT_` environment overrides on top.
  pub fn load(maybe_path: Option<&Path>) -> Result<Self, ConfigError> {
    if let Some(path) = maybe_path
      && !path.is_file()
    {
      return Err(ConfigError::MissingFile(path.display().to_string()));
    }
    let mut figment = Figment::new();
    if let Some(path) = maybe_path {
      figment = figment.merge(Toml::file(path));
    }
    Self::from_figment(figment.merge(env_provider()))
  }

  /// Parses a configuration from TOML text only (no environment); used by tests and tooling.
  pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
    Self::from_figment(Figment::new().merge(Toml::string(text)))
  }

  fn from_figment(figment: Figment) -> Result<Self, ConfigError> {
    figment.extract().map_err(|e| ConfigError::Invalid(Box::new(e)))
  }
}

impl DatabaseConfig {
  pub fn acquire_timeout(&self) -> Duration {
    Duration::from_secs(self.acquire_timeout_secs)
  }
}

impl Default for ServerConfig {
  fn default() -> Self {
    Self {
      listen_addr: SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 8080),
      worker_id: 0,
      partitions_ahead_months: 3,
    }
  }
}

impl Default for OidcSyncConfig {
  fn default() -> Self {
    Self {
      is_enabled: false,
      interval_secs: 300,
      initial_delay_secs: 30,
      maybe_client_secret: None,
      maybe_admin_url: None,
      page_size: 100,
      sync_groups: false,
    }
  }
}

impl Default for RoleNames {
  fn default() -> Self {
    Self {
      owner: "owner".into(),
      admin: "admin".into(),
      member: "member".into(),
      guest: "guest".into(),
    }
  }
}

impl Default for PolicyConfig {
  fn default() -> Self {
    Self {
      guests_dm_only_with_shared_conversations: true,
      invites_require_verified_email: true,
    }
  }
}

impl Default for LimitsConfig {
  fn default() -> Self {
    Self {
      max_ciphertext_bytes: 1024 * 1024,
      max_mentions_per_message: 200,
      max_group_dm_participants: 8,
      max_name_length: 100,
      max_text_length: 1000,
      default_page_size: 50,
      max_page_size: 200,
      max_directory_results: 50,
      max_avatar_bytes: 1024 * 1024,
      max_background_bytes: 8 * 1024 * 1024,
      max_backgrounds_per_user: 24,
      org_recovery_request_ttl_secs: 86_400,
      max_recovery_blob_bytes: 8192,
      max_profile_text_chars: 100,
      max_user_group_members: 5000,
      max_unread_counted: 1000,
      max_key_packages_per_device: 200,
      max_key_package_bytes: 16 * 1024,
      max_key_packages_per_upload: 100,
      max_mls_payload_bytes: 1024 * 1024,
      max_welcomes_per_commit: 500,
      max_key_bag_bytes: 1024 * 1024,
      max_device_list_bytes: 64 * 1024,
      max_devices_per_user: 20,
      max_pending_attachments_per_user: 20,
      max_attachment_bytes_per_user: 20 * 1024 * 1024 * 1024,
      max_meeting_mails_per_user_per_hour: 200,
    }
  }
}

impl Default for GatewayConfig {
  fn default() -> Self {
    Self {
      heartbeat_interval_secs: 30,
      heartbeat_timeout_secs: 90,
      resume_window_secs: 120,
      resume_buffer_events: 1000,
      identify_timeout_secs: 10,
      connection_max_age_secs: 3600,
      presence_offline_grace_secs: 5,
      max_frame_bytes: 64 * 1024,
      max_client_frames_per_minute: 300,
      client_frame_burst: 60,
      max_sessions_per_device: 4,
      max_sessions_per_user: 32,
    }
  }
}

impl Default for PushConfig {
  fn default() -> Self {
    Self {
      maybe_vapid_private_key: None,
      vapid_subject: "mailto:admin@example.com".into(),
      ttl_secs: 3600,
      request_timeout_secs: 10,
      max_concurrent_sends: 32,
      allow_private_endpoints: false,
    }
  }
}

impl Default for MailTexts {
  fn default() -> Self {
    Self {
      invitation_subject: "Invitation: {title}".into(),
      invitation_body: "{organizer} invites you to \"{title}\".\n\nStart: {start}\nEnd: {end}\n\n{description}\nJoin: {join_url}\n".into(),
      update_subject: "Updated: {title}".into(),
      update_body: "{organizer} changed \"{title}\".\n\nStart: {start}\nEnd: {end}\n\n{description}\nJoin: {join_url}\n".into(),
      cancellation_subject: "Cancelled: {title}".into(),
      cancellation_body: "{organizer} cancelled \"{title}\" ({start}).\n".into(),
    }
  }
}

impl Default for NotificationsConfig {
  fn default() -> Self {
    Self {
      read_receipts_default: true,
      urgent_repeat_interval_secs: 120,
      urgent_max_pushes: 5,
      urgent_scheduler_interval_secs: 15,
      mobile_push_delay_secs: 120,
    }
  }
}

impl Default for RetentionJobConfig {
  fn default() -> Self {
    Self {
      is_enabled: true,
      interval_secs: 3600,
      initial_delay_secs: 60,
      partitions_ahead_months: 3,
    }
  }
}

impl Default for RendezvousConfig {
  fn default() -> Self {
    Self {
      ttl_secs: 300,
      max_frame_bytes: 16 * 1024,
      max_total_bytes: 256 * 1024,
      max_pending_frames: 8,
      max_per_user: 5,
      join_timeout_secs: 10,
    }
  }
}

impl Default for LoggingConfig {
  fn default() -> Self {
    Self {
      filter: "info".into(),
      format: LogFormat::Pretty,
    }
  }
}

fn env_provider() -> Env {
  Env::prefixed(ENV_PREFIX).ignore(&["config"]).split(ENV_NESTING)
}

fn default_join_token_ttl_secs() -> u64 {
  60
}

fn default_guest_grace_secs() -> u64 {
  2 * 3600
}

fn default_smtp_port() -> u16 {
  587
}

fn default_smtp_timeout_secs() -> u64 {
  20
}

fn default_max_meeting_invitees() -> usize {
  200
}

fn default_caldav_timeout_secs() -> u64 {
  10
}

fn default_ring_timeout_secs() -> u64 {
  30
}

fn default_call_idle_secs() -> u64 {
  12 * 3600
}

fn default_region() -> String {
  "us-east-1".into()
}

fn default_true() -> bool {
  true
}

fn default_presign_expiry_secs() -> u64 {
  900
}

fn default_max_attachment_bytes() -> u64 {
  100 * 1024 * 1024
}

fn default_max_connections() -> u32 {
  10
}

fn default_acquire_timeout_secs() -> u64 {
  5
}

fn default_client_id() -> String {
  "teamcraft".into()
}

fn default_server_client() -> String {
  "teamcraft-server".into()
}

fn default_algorithms() -> Vec<String> {
  vec!["RS256".into(), "ES256".into(), "EdDSA".into()]
}

fn default_leeway_secs() -> u64 {
  30
}

fn default_jwks_cache_ttl_secs() -> u64 {
  3600
}

fn default_jwks_min_refresh_interval_secs() -> u64 {
  30
}

fn default_http_timeout_secs() -> u64 {
  10
}

#[cfg(test)]
mod tests {
  use std::net::SocketAddr;

  use secrecy::ExposeSecret;

  use super::*;

  #[test]
  fn the_web_policy_allows_only_the_origins_the_app_needs() {
    let mut web = WebConfig::default();
    let policy = web.resolved_content_security_policy();
    assert!(policy.contains("connect-src 'self';"), "{policy}");
    assert!(!policy.contains("https:") && !policy.contains("wss:"), "{policy}");

    web.allow_connections_to(&[
      "https://auth.example.com/realms/company",
      "https://sfu.example.com:4433/call",
      "http://storage.example.com:3900",
      "https://auth.example.com/other-path",
      "not a url",
    ]);
    let policy = web.resolved_content_security_policy();
    assert!(
      policy.contains(
        "connect-src 'self' https://auth.example.com https://sfu.example.com:4433 http://storage.example.com:3900;"
      ),
      "{policy}"
    );
    assert!(web.validate().is_ok());
  }

  #[test]
  fn a_broken_web_policy_is_refused_at_startup() {
    let with_policy = |policy: &str| WebConfig {
      content_security_policy: policy.into(),
      ..WebConfig::default()
    };
    assert!(
      with_policy("default-src 'self'\nscript-src *")
        .validate()
        .is_err(),
      "a newline"
    );
    assert!(with_policy("  ").validate().is_err());
    let with_origins = |origins: &[&str]| WebConfig {
      connect_origins: origins.iter().map(|o| (*o).to_owned()).collect(),
      ..WebConfig::default()
    };
    assert!(with_origins(&["https://ok.example.com", "*"]).validate().is_err());
    assert!(
      with_origins(&["https://a.example.com; script-src *"])
        .validate()
        .is_err()
    );
    assert!(
      with_origins(&["https://ok.example.com", "wss://ok.example.com"])
        .validate()
        .is_ok()
    );
    // A policy written out without the placeholder is used as written.
    assert_eq!(
      with_policy("default-src 'none'").resolved_content_security_policy(),
      "default-src 'none'"
    );
  }

  const MINIMAL: &str = r#"
    [database]
    url = "postgres://u:p@localhost/db"
    [oidc]
    issuer = "https://auth.example.com/realms/company"
  "#;

  #[test]
  fn minimal_config_gets_documented_defaults() {
    let config = Config::from_toml(MINIMAL).unwrap();
    assert_eq!(
      config.server.listen_addr,
      "[::]:8080".parse::<SocketAddr>().unwrap()
    );
    assert_eq!(config.oidc.client_id, "teamcraft");
    assert_eq!(config.oidc.roles.guest, "guest");
    assert_eq!(config.database.max_connections, 10);
    assert_eq!(config.logging.format, LogFormat::Pretty);
    assert!(config.oidc.maybe_jwks_uri.is_none());
  }

  #[test]
  fn example_file_parses_and_matches_the_defaults() {
    let text = include_str!("../../../teamcraft.example.toml");
    let config = Config::from_toml(text).unwrap();
    assert_eq!(config.limits.max_page_size, LimitsConfig::default().max_page_size);
    assert_eq!(
      config.gateway.resume_buffer_events,
      GatewayConfig::default().resume_buffer_events
    );
    assert_eq!(
      config.gateway.max_frame_bytes,
      GatewayConfig::default().max_frame_bytes
    );
    assert_eq!(
      config.policy.guests_dm_only_with_shared_conversations,
      PolicyConfig::default().guests_dm_only_with_shared_conversations
    );
    assert_eq!(
      config.rendezvous.max_total_bytes,
      RendezvousConfig::default().max_total_bytes
    );
    assert_eq!(config.server.listen_addr, ServerConfig::default().listen_addr);
  }

  #[test]
  fn listen_addr_accepts_both_families() {
    let v6 = format!("{MINIMAL}\n[server]\nlisten_addr = \"[::1]:9000\"\n");
    assert!(Config::from_toml(&v6).unwrap().server.listen_addr.is_ipv6());
    let v4 = format!("{MINIMAL}\n[server]\nlisten_addr = \"127.0.0.1:9000\"\n");
    assert!(Config::from_toml(&v4).unwrap().server.listen_addr.is_ipv4());
  }

  #[test]
  fn role_names_and_client_are_configurable() {
    let text = format!("{MINIMAL}\n[oidc.roles]\nguest = \"external\"\n");
    let config = Config::from_toml(&text).unwrap();
    assert_eq!(config.oidc.roles.guest, "external");
    assert_eq!(config.oidc.roles.owner, "owner");
  }

  #[test]
  fn missing_required_values_are_rejected() {
    assert!(Config::from_toml("[oidc]\nissuer = \"https://auth.example.com\"").is_err());
    assert!(Config::from_toml("[database]\nurl = \"postgres://x\"").is_err());
  }

  #[test]
  fn unknown_keys_are_rejected() {
    let text = format!("{MINIMAL}\n[server]\nlisten_adress = \"0.0.0.0:1\"\n");
    assert!(Config::from_toml(&text).is_err());
  }

  #[test]
  #[allow(clippy::result_large_err)] // figment::Jail closure signature is fixed by the library
  fn environment_overrides_file_values() {
    figment::Jail::expect_with(|jail| {
      jail.create_file("teamcraft.toml", MINIMAL)?;
      jail.set_env("TEAMCRAFT_OIDC__ISSUER", "https://other.example.com/realms/x");
      jail.set_env("TEAMCRAFT_OIDC__ROLES__GUEST", "external");
      jail.set_env("TEAMCRAFT_SERVER__WORKER_ID", "7");
      jail.set_env("TEAMCRAFT_CONFIG", "ignored.toml");

      let config = Config::load(Some(Path::new("teamcraft.toml"))).map_err(|e| e.to_string())?;
      assert_eq!(config.oidc.issuer, "https://other.example.com/realms/x");
      assert_eq!(config.oidc.roles.guest, "external");
      assert_eq!(config.server.worker_id, 7);
      assert_eq!(config.database.url.expose_secret(), "postgres://u:p@localhost/db");
      Ok(())
    });
  }

  #[test]
  fn missing_config_file_is_an_error() {
    assert!(matches!(
      Config::load(Some(Path::new("/nonexistent/teamcraft.toml"))),
      Err(ConfigError::MissingFile(_))
    ));
  }
}
