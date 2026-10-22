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
  pub integration: IntegrationConfig,
  #[serde(default)]
  pub web: WebConfig,
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

/// Serving the web client (the `trunk build` output of `teamcraft-ui`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WebConfig {
  /// Directory with `index.html` and the hashed assets; without it the server only serves the API.
  #[serde(rename = "static_dir")]
  pub maybe_static_dir: Option<PathBuf>,
  /// `Content-Security-Policy` of every page and asset. The default allows the app's own origin,
  /// WebAssembly, WebSocket/WebTransport connections and `blob:`/`data:` images and media.
  pub content_security_policy: String,
}

impl Default for WebConfig {
  fn default() -> Self {
    Self {
      maybe_static_dir: None,
      content_security_policy: "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; \
        img-src 'self' data: blob:; media-src 'self' blob:; font-src 'self'; connect-src 'self' wss: https:; \
        worker-src 'self' blob:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"
        .into(),
    }
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
      max_key_packages_per_device: 200,
      max_key_package_bytes: 16 * 1024,
      max_key_packages_per_upload: 100,
      max_mls_payload_bytes: 1024 * 1024,
      max_welcomes_per_commit: 500,
      max_key_bag_bytes: 1024 * 1024,
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
