use std::sync::Arc;

use teamcraft_db::{Database, SnowflakeGenerator};

use crate::auth::{Revocations, TokenValidator};
use secrecy::SecretString;

use teamcraft_crypto::server_secret::SettingsSealingKey;

use crate::brand::BrandStore;
use crate::config::{
  BrandConfig, BrandingConfig, CalDavConfig, GatewayConfig, GuestsConfig, IntegrationConfig, LimitsConfig,
  LocalAccountsConfig, MailConfig, MailTexts, NotificationsConfig, PolicyConfig, PresenceConfig,
  RendezvousConfig, RetentionJobConfig, RoleNames, SecurityConfig, SharesConfig, SignInPageConfig,
  SmtpConfig, WebConfig,
};
use crate::error::ApiError;
use crate::gateway::Hub;
use crate::meetings::MeetingService;
use crate::meetings::mailer::Mailer;
use crate::push::{PushDispatcher, PushTracker};
use crate::rendezvous::RendezvousHub;
use crate::runtime::mail_status::MailStatus;
use crate::runtime::{SettingsCell, Slot};
use crate::security::RateLimits;
use crate::services::calls::CallService;
use crate::services::local_accounts::LocalAuth;
use crate::services::permission_scheme::SchemeStore;
use crate::services::session::PolicyHolder;
use crate::storage::BlobStore;

/// The scope every sign-in asks for.
const OPENID_SCOPE: &str = "openid";

/// Shared, cheaply cloneable application state of all handlers.
#[derive(Clone)]
pub struct AppState {
  pub database: Database,
  pub validator: Arc<TokenValidator>,
  /// Sessions the identity provider ended (back-channel logout).
  pub revocations: Arc<Revocations>,
  pub ids: Arc<SnowflakeGenerator>,
  pub hub: Arc<Hub>,
  pub rendezvous: Arc<RendezvousHub>,
  /// Shutdown in progress: readiness fails and WebSockets close with 1012.
  pub drain: Arc<crate::drain::Drain>,
  pub push: Arc<PushDispatcher>,
  /// What was pushed to which device: withdrawals, rings, delivery counts, test limits.
  pub push_tracker: Arc<PushTracker>,
  /// Attachment storage, if configured.
  pub maybe_blob_store: Option<Arc<BlobStore>>,
  /// Call join tokens, if calls are configured.
  pub maybe_calls: Option<Arc<CallService>>,
  /// Meetings, if configured.
  pub maybe_meetings: Option<Arc<MeetingService>>,
  /// Outgoing e-mail of the server (invitations, activations, digests and, in production, meeting
  /// invitations): from the admin console, else `[mail.smtp]`. Empty: no mail is sent.
  pub mailer: Arc<Slot<dyn Mailer>>,
  /// How that mail went since the start.
  pub mail_status: Arc<MailStatus>,
  /// Encrypts passwords entered in the admin console; `None` without `[secrets]`.
  pub maybe_settings_key: Option<Arc<SettingsSealingKey>>,
  /// Serializes changes of runtime settings (brands, mail, calendar, instance settings), so each
  /// one is validated against the state it replaces.
  pub settings_lock: Arc<tokio::sync::Mutex<()>>,
  /// Local guest accounts, if enabled.
  pub maybe_local: Option<Arc<LocalAuth>>,
  /// The signed workspace policy in force.
  pub policy: Arc<PolicyHolder>,
  pub scheme: Arc<SchemeStore>,
  /// The settings in force: the configuration with the admin console's changes on top.
  pub settings: Arc<SettingsCell>,
  pub rate_limits: Arc<RateLimits>,
  /// The brands and the host names that select them.
  pub brands: Arc<BrandStore>,
  /// Permits for decoding uploaded pictures (`limits.max_concurrent_image_decodes`).
  pub image_decodes: Arc<tokio::sync::Semaphore>,
}

/// Runtime settings handlers need, taken from the configuration.
#[derive(Debug, Clone, Default)]
pub struct Settings {
  /// The OIDC sync, if enabled.
  pub maybe_oidc_sync: Option<OidcSyncSettings>,
  pub policy: PolicyConfig,
  pub limits: LimitsConfig,
  pub gateway: GatewayConfig,
  pub rendezvous: RendezvousConfig,
  pub retention: RetentionJobConfig,
  pub notifications: NotificationsConfig,
  pub integration: IntegrationConfig,
  pub web: WebConfig,
  pub security: SecurityConfig,
  pub guests: GuestsConfig,
  pub mail: MailConfig,
  pub local_accounts: LocalAccountsConfig,
  pub sign_in_page: SignInPageConfig,
  pub presence: PresenceConfig,
  pub shares: SharesConfig,
  pub push: PushSettings,
  /// Token claim the organization is read from, if any.
  pub maybe_organization_claim: Option<String>,
  /// How the apps sign in, announced in `GET /api/v1/server-info`.
  pub sign_in: SignInSettings,
  pub branding: BrandingConfig,
  pub brands: Vec<BrandConfig>,
  /// Largest attachment a client may announce (`[storage].max_attachment_bytes`).
  pub max_attachment_bytes: u64,
  /// Meeting invitations: most invitees and the mail texts.
  pub meeting_mail: MeetingMailConfig,
  /// The mail server of the configuration file (`[mail.smtp]`, else `[meetings.smtp]`).
  pub maybe_mail_seed: Option<SmtpConfig>,
  /// The calendar of the configuration file (`[meetings.caldav]`).
  pub maybe_calendar_seed: Option<CalDavConfig>,
}

/// Limits and texts of meeting invitations.
#[derive(Debug, Clone)]
pub struct MeetingMailConfig {
  pub max_invitees: usize,
  pub texts: MailTexts,
}

impl Default for MeetingMailConfig {
  fn default() -> Self {
    Self {
      max_invitees: 200,
      texts: MailTexts::default(),
    }
  }
}

impl AppState {
  /// A fresh server-assigned ID.
  pub fn next_id<T: teamcraft_proto::Id>(&self) -> Result<T, ApiError> {
    self.ids.next_id().map_err(|e| ApiError::Internal(e.to_string()))
  }
}

/// Everything the OIDC sync needs to talk to the identity provider.
#[derive(Debug, Clone)]
pub struct OidcSyncSettings {
  pub issuer: String,
  pub token_url: String,
  pub admin_url: String,
  pub client_id: String,
  pub server_client: String,
  pub client_secret: SecretString,
  pub roles: RoleNames,
  pub page_size: u32,
  pub sync_groups: bool,
}

/// The public part of `[oidc]` the apps need to sign in (docs/operations.md "Server address in
/// the apps"). Nothing secret: it is served to anyone.
#[derive(Debug, Clone, Default)]
pub struct SignInSettings {
  pub issuer: String,
  pub client_id: String,
  /// Always starts with `openid`.
  pub scopes: Vec<String>,
  pub maybe_redirect_uris: Option<Vec<String>>,
}

impl SignInSettings {
  pub fn new(
    issuer: String,
    client_id: String,
    configured_scopes: &[String],
    maybe_redirect_uris: Option<Vec<String>>,
  ) -> Self {
    let mut scopes = vec![OPENID_SCOPE.to_owned()];
    for scope in configured_scopes {
      if !scopes.contains(scope) {
        scopes.push(scope.clone());
      }
    }
    Self {
      issuer,
      client_id,
      scopes,
      maybe_redirect_uris,
    }
  }
}

/// What the message routing needs from the push configuration.
#[derive(Debug, Clone)]
pub struct PushSettings {
  pub ttls: PushTtls,
  pub retry: RetryPolicy,
  /// The shortest time between two test pushes to one device.
  pub test_interval: std::time::Duration,
  /// Rings one caller may push per minute.
  pub max_rings_per_minute: u32,
  /// How long withdrawals for one conversation are collected before one goes out.
  pub dismiss_coalesce: std::time::Duration,
  pub allow_private_endpoints: bool,
}

/// How long push services may hold a wake-up, per kind (notifications.md, "TTL per kind").
#[derive(Debug, Clone, Copy)]
pub struct PushTtls {
  pub message: std::time::Duration,
  pub call: std::time::Duration,
  pub dismiss: std::time::Duration,
  pub test: std::time::Duration,
}

/// Retries of pushes that failed for a temporary reason, through the durable queue
/// (notifications.md, "Mobile push delivery"). The wake-up's TTL bounds them all.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
  /// Attempts after the first one; `0` sends once and gives up.
  pub max_retries: u32,
  /// Wait before the first retry; doubles for each further one, with jitter.
  pub initial_delay: std::time::Duration,
  /// The longest wait between two attempts (a push service's `Retry-After` may ask for more).
  pub max_delay: std::time::Duration,
  /// Workers per pool (APNs/FCM, and the endpoints users chose); each holds a database connection
  /// while it sends.
  pub workers: usize,
  /// Wake-ups one device may have waiting; more are dropped until the queue drains.
  pub max_queued_per_device: u32,
  /// How often idle workers look for due wake-ups.
  pub poll_interval: std::time::Duration,
}

impl PushTtls {
  pub fn for_kind(&self, kind: &teamcraft_proto::PushKind) -> std::time::Duration {
    use teamcraft_proto::PushKind;
    match kind {
      PushKind::Call => self.call,
      PushKind::Dismiss => self.dismiss,
      PushKind::Test => self.test,
      PushKind::Message | PushKind::Mention | PushKind::Unknown(_) => self.message,
    }
  }
}

impl Default for PushSettings {
  fn default() -> Self {
    use std::time::Duration;
    Self {
      ttls: PushTtls {
        message: Duration::from_secs(86_400),
        call: Duration::from_secs(30),
        dismiss: Duration::from_secs(86_400),
        test: Duration::from_secs(300),
      },
      retry: RetryPolicy {
        max_retries: 200,
        initial_delay: Duration::from_secs(2),
        max_delay: Duration::from_secs(600),
        workers: 2,
        max_queued_per_device: 200,
        poll_interval: Duration::from_secs(1),
      },
      test_interval: Duration::from_secs(10),
      max_rings_per_minute: 10,
      dismiss_coalesce: Duration::from_millis(3000),
      allow_private_endpoints: false,
    }
  }
}
