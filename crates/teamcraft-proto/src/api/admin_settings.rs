//! Runtime settings of the admin console (admin.md §6): brands, outgoing mail, the calendar server
//! and the instance settings that used to need the configuration file. The file stays the seed: a
//! setting nobody changed in the console comes from it (`source = config`), a changed one is stored
//! in the database (`source = database`) and can be reset to the file's value.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::wire_enum::wire_enum;
use crate::{
  BrandAssetSlot, BrandColors, BrandLinks, BrandThemePolicy, ConversationId, CrashReportSettings,
  HealthAlertSettings, NotificationLevel, SignInMethod, SpaceId, UnixMillis,
};

/// The application and bundle ID of the official apps.
pub const OFFICIAL_APP_ID: &str = "de.zeitgleich.teamcraft";
/// The official Android apps: the Play build and the one without Google services.
pub const OFFICIAL_ANDROID_PACKAGES: [&str; 2] = [OFFICIAL_APP_ID, "de.zeitgleich.teamcraft.libre"];
/// The certificate every official Android build is signed with, Play App Signing included
/// (apps/android/README.md, "Signing"). Public: it is in every published APK.
pub const OFFICIAL_ANDROID_CERT_FINGERPRINT: &str =
  "68:60:BF:07:4F:47:08:19:82:4F:92:A9:E6:57:30:5A:7A:30:81:49:2C:FD:C2:55:07:81:02:08:5E:50:05:D7";
/// The Apple team of the official iOS app; public once the app is published. Empty until then: no
/// association file for Apple, unless an admin sets a team (docs/app-links.md, "Open points").
pub const OFFICIAL_APPLE_TEAM_ID: &str = "";

/// Default of [`LimitSettings::max_space_image_bytes`]: a 256×256 PNG stays well below it.
pub const DEFAULT_MAX_SPACE_IMAGE_BYTES: u64 = 1024 * 1024;

wire_enum! {
  /// Where the value in force comes from.
  pub enum SettingSource {
    /// The configuration file, or the built-in default when the file does not set it.
    Config => "config",
    /// Changed in the admin console; stored in the database.
    Database => "database",
  }
}

wire_enum! {
  /// Where a brand is defined.
  pub enum BrandSource {
    /// The built-in "TeamCraft" brand used when no other brand is the default.
    Builtin => "builtin",
    /// `[[brands]]` in the configuration file; read-only in the console until taken over.
    Config => "config",
    /// Created or taken over in the admin console.
    Database => "database",
  }
}

wire_enum! {
  /// How the connection to the mail server is secured.
  pub enum SmtpSecurityMode {
    /// STARTTLS on the submission port; the upgrade is required.
    StartTls => "start_tls",
    /// TLS from the first byte (port 465).
    Tls => "tls",
    /// No encryption; only for a mail catcher on the same machine.
    Plain => "none",
  }
}

wire_enum! {
  /// A group of instance settings that is changed and reset as a whole.
  pub enum RuntimeSettingsKey {
    ServerRules => "server_rules",
    Notifications => "notifications",
    EmailDigest => "email_digest",
    Shares => "shares",
    Limits => "limits",
    Integration => "integration",
    LocalAccounts => "local_accounts",
    SignInPage => "sign_in_page",
    MailTexts => "mail_texts",
    MeetingMail => "meeting_mail",
    TokenBinding => "token_binding",
    AppLinks => "app_links",
    HealthAlerts => "health_alerts",
    Holidays => "holidays",
    CrashReports => "crash_reports",
  }
}

wire_enum! {
  /// Whether access tokens must be bound to the device's key (DPoP, RFC 9449; e2ee.md §3.1.2).
  pub enum DpopPolicy {
    /// Nothing is checked, bound tokens count as bearer tokens: the way back if proofs fail for
    /// everybody.
    Off => "off",
    /// Proofs that are sent are checked and bound tokens need one; unbound tokens still work, so
    /// clients without DPoP keep working. The default.
    Optional => "optional",
    /// Tokens of the identity provider must be bound. Closes token relay by a look-alike server.
    Required => "required",
  }
}

// --- Brands -----------------------------------------------------------------------------------

/// `GET /api/v1/admin/brands`: every brand with its source, and the host names the server is
/// deployed for (only those can be given to a brand, because the proxy and TLS layer must know them).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminBrandsResponse {
  /// Default brand first.
  pub brands: Vec<AdminBrand>,
  /// Host names a brand may use, with the brand that uses each one now.
  pub served_hostnames: Vec<ServedHostname>,
  /// Whether images can be uploaded (they are kept in the object storage).
  pub is_image_upload_available: bool,
  pub max_asset_bytes: u64,
  /// Whether the server has a key to store a brand's mail server password encrypted (`[secrets]` in
  /// the configuration).
  #[serde(default)]
  pub can_store_secrets: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminBrand {
  pub id: String,
  pub source: BrandSource,
  pub is_default: bool,
  /// A database brand that replaces a configuration brand with the same id; deleting it brings
  /// the configuration brand back.
  pub is_overriding_config: bool,
  pub settings: BrandSettings,
  /// Slots that have an image.
  #[serde(default)]
  pub assets: Vec<BrandAssetSlot>,
  #[serde(rename = "updated_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_updated_at: Option<UnixMillis>,
  /// The brand's own mail server. None: its mail goes through the instance's.
  #[serde(rename = "mail_server", default, skip_serializing_if = "Option::is_none")]
  pub maybe_mail_server: Option<BrandMailServer>,
}

/// What an admin can change about a brand. Brand-specific content security policies and extra
/// connect origins stay in the configuration file (admin.md §6.4).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BrandSettings {
  pub display_name: String,
  /// The organization behind the brand ("Brand A Ltd."), shown in the footer of the sign-in page.
  #[serde(
    rename = "organization_name",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_organization_name: Option<String>,
  /// Lower-case DNS names, each one of the served host names; the first is the canonical one.
  #[serde(default)]
  pub hostnames: Vec<String>,
  /// People with an address in one of these domains have this brand as their home brand.
  #[serde(default)]
  pub email_domains: Vec<String>,
  #[serde(default)]
  pub colors: BrandColors,
  /// Language code -> text; `en` is the fallback.
  #[serde(default)]
  pub login_title: BTreeMap<String, String>,
  #[serde(default)]
  pub login_text: BTreeMap<String, String>,
  #[serde(default)]
  pub links: BrandLinks,
  #[serde(default)]
  pub mail: BrandMailSettings,
  /// The provider part of the sign-in button "Sign in with …" (e.g. "Microsoft"); else the
  /// display name. Its icon is the `sso_icon` image.
  #[serde(rename = "sso_label", default, skip_serializing_if = "Option::is_none")]
  pub maybe_sso_label: Option<String>,
  /// The default colour theme and which themes people may pick (docs/branding.md §5.1). In a
  /// request, absent keeps the stored policy (an older console doesn't know it) and an unrestricted
  /// one removes it. Absent in an answer: the brand has none.
  #[serde(rename = "theme_policy", default, skip_serializing_if = "Option::is_none")]
  pub maybe_theme_policy: Option<BrandThemePolicy>,
}

/// The sender of mail that goes out as this brand.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BrandMailSettings {
  #[serde(rename = "sender_name", default, skip_serializing_if = "Option::is_none")]
  pub maybe_sender_name: Option<String>,
  /// Else the default sender address of the mail settings.
  #[serde(rename = "from_address", default, skip_serializing_if = "Option::is_none")]
  pub maybe_from_address: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ServedHostname {
  pub hostname: String,
  /// The brand this host name selects now; none: requests to it are treated as unknown hosts.
  #[serde(rename = "brand_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_brand_id: Option<String>,
}

/// `POST /api/v1/admin/brands`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateBrandRequest {
  /// `[a-z0-9-]`, 1 to 32 characters, unused by any other brand.
  pub id: String,
  #[serde(default)]
  pub is_default: bool,
  pub settings: BrandSettings,
}

/// `PUT /api/v1/admin/brands/{brand_id}`: replaces the settings of a database brand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UpdateBrandRequest {
  #[serde(default)]
  pub is_default: bool,
  pub settings: BrandSettings,
}

// --- Mail -------------------------------------------------------------------------------------

/// `GET /api/v1/admin/settings/mail`: the mail server all of the server's own mail goes through
/// (invitations, activations, digests, meeting invitations). The password is never sent back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminMailSettingsResponse {
  pub source: SettingSource,
  /// None: no mail is sent.
  #[serde(rename = "smtp", default, skip_serializing_if = "Option::is_none")]
  pub maybe_smtp: Option<SmtpSettings>,
  /// Whether the server has a key to store passwords encrypted (`[secrets]` in the configuration).
  pub can_store_secrets: bool,
  pub status: MailDeliveryStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SmtpSettings {
  pub host: String,
  pub port: u16,
  pub security: SmtpSecurityMode,
  #[serde(rename = "username", default, skip_serializing_if = "Option::is_none")]
  pub maybe_username: Option<String>,
  pub has_password: bool,
  /// Default sender address; brands may have their own.
  pub from_address: String,
  #[serde(rename = "from_name", default, skip_serializing_if = "Option::is_none")]
  pub maybe_from_name: Option<String>,
}

/// A brand's own SMTP server (admin.md §6.2). The sender stays in the brand's settings; the password
/// is never sent back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BrandMailServer {
  pub host: String,
  pub port: u16,
  pub security: SmtpSecurityMode,
  #[serde(rename = "username", default, skip_serializing_if = "Option::is_none")]
  pub maybe_username: Option<String>,
  pub has_password: bool,
  /// How mail through this server went, since the server started.
  #[serde(default)]
  pub status: MailDeliveryStatus,
}

/// `PUT /api/v1/admin/brands/{brand_id}/mail-server`. An empty host removes the brand's own server,
/// like `DELETE`: its mail goes through the instance's mail server again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UpdateBrandMailServerRequest {
  pub host: String,
  pub port: u16,
  pub security: SmtpSecurityMode,
  #[serde(rename = "username", default, skip_serializing_if = "Option::is_none")]
  pub maybe_username: Option<String>,
  /// Left out: the stored password stays.
  #[serde(default)]
  pub password: SecretUpdate,
}

/// `PUT /api/v1/admin/settings/mail`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UpdateMailSettingsRequest {
  pub host: String,
  pub port: u16,
  pub security: SmtpSecurityMode,
  #[serde(rename = "username", default, skip_serializing_if = "Option::is_none")]
  pub maybe_username: Option<String>,
  pub password: SecretUpdate,
  pub from_address: String,
  #[serde(rename = "from_name", default, skip_serializing_if = "Option::is_none")]
  pub maybe_from_name: Option<String>,
}

/// How the last mail went, since the server started.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MailDeliveryStatus {
  #[serde(rename = "last_success_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_success_at: Option<UnixMillis>,
  #[serde(rename = "last_failure_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_failure_at: Option<UnixMillis>,
  /// What the mail server said at the last failure.
  #[serde(rename = "last_error", default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_error: Option<String>,
}

/// `POST /api/v1/admin/settings/mail/test`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SendTestMailRequest {
  /// Else the caller's own address.
  #[serde(rename = "to_address", default, skip_serializing_if = "Option::is_none")]
  pub maybe_to_address: Option<String>,
}

/// The answer of a connection test (mail or calendar).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SettingsTestResponse {
  pub is_ok: bool,
  #[serde(rename = "error", default, skip_serializing_if = "Option::is_none")]
  pub maybe_error: Option<String>,
}

/// A write-only secret in a settings request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "action", content = "value", rename_all = "snake_case")]
pub enum SecretUpdate {
  /// Leave the stored secret as it is.
  #[default]
  Keep,
  /// Remove it.
  Clear,
  /// Replace it.
  Set(WriteOnlySecret),
}

/// A secret that goes from the admin to the server and never back. Not printed, cleared on drop.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Zeroize, ZeroizeOnDrop)]
#[serde(transparent)]
pub struct WriteOnlySecret(String);

// --- Calendar ---------------------------------------------------------------------------------

/// `GET /api/v1/admin/settings/calendar`: the CalDAV calendar meetings are written to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminCalendarSettingsResponse {
  /// Meetings are configured (`[meetings]`); without them there is nothing to store.
  pub is_available: bool,
  pub source: SettingSource,
  #[serde(rename = "caldav", default, skip_serializing_if = "Option::is_none")]
  pub maybe_caldav: Option<CalDavSettings>,
  pub can_store_secrets: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CalDavSettings {
  pub calendar_url: String,
  pub username: String,
  pub has_password: bool,
  /// Where one user's calendar lives (`{email}` is their address), for "in a meeting" statuses.
  #[serde(
    rename = "free_busy_url_template",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_free_busy_url_template: Option<String>,
}

/// `PUT /api/v1/admin/settings/calendar`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UpdateCalendarSettingsRequest {
  pub calendar_url: String,
  pub username: String,
  pub password: SecretUpdate,
  #[serde(
    rename = "free_busy_url_template",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_free_busy_url_template: Option<String>,
}

// --- Instance settings ------------------------------------------------------------------------

/// `GET /api/v1/admin/settings`: the instance settings in force, and where each group comes from.
/// New groups are new fields, so older clients ignore them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RuntimeSettingsResponse {
  pub server_rules: ServerRulesSettings,
  pub notifications: NotificationRuntimeSettings,
  pub email_digest: EmailDigestSettings,
  pub shares: ShareSettings,
  pub limits: LimitSettings,
  pub integration: IntegrationSettings,
  pub local_accounts: LocalAccountSettings,
  pub sign_in_page: SignInPageSettings,
  pub mail_texts: MailTextSettings,
  pub meeting_mail: MeetingMailSettings,
  /// Absent from older servers: the defaults, which is what they did not check either way.
  #[serde(default)]
  pub token_binding: TokenBindingSettings,
  /// Absent from older servers: the defaults, the official apps.
  #[serde(default)]
  pub app_links: AppLinkSettings,
  /// Absent from older servers: the defaults.
  #[serde(default)]
  pub health_alerts: HealthAlertSettings,
  /// Absent from older servers: no region, nothing is skipped.
  #[serde(default)]
  pub holidays: HolidaySettings,
  /// Absent from older servers: off.
  #[serde(default)]
  pub crash_reports: CrashReportSettings,
  /// Host of the publisher's crash reports service built into this server; absent: the build has
  /// none, and the mode `publisher` sends nothing.
  #[serde(
    rename = "publisher_crash_reports_host",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_publisher_crash_reports_host: Option<String>,
  /// How the holiday feed stands; absent without a feed (and from older servers).
  #[serde(rename = "holiday_feed", default, skip_serializing_if = "Option::is_none")]
  pub maybe_holiday_feed: Option<HolidayFeedStatus>,
  /// One entry per group.
  pub sources: Vec<RuntimeSettingsState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RuntimeSettingsState {
  pub key: RuntimeSettingsKey,
  pub source: SettingSource,
  #[serde(rename = "updated_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_updated_at: Option<UnixMillis>,
}

/// One group of instance settings with all its values; `PUT /api/v1/admin/settings` replaces it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "section", content = "values", rename_all = "snake_case")]
pub enum RuntimeSettingsSection {
  ServerRules(ServerRulesSettings),
  Notifications(NotificationRuntimeSettings),
  EmailDigest(EmailDigestSettings),
  Shares(ShareSettings),
  Limits(LimitSettings),
  Integration(IntegrationSettings),
  LocalAccounts(LocalAccountSettings),
  SignInPage(SignInPageSettings),
  MailTexts(MailTextSettings),
  MeetingMail(MeetingMailSettings),
  TokenBinding(TokenBindingSettings),
  AppLinks(AppLinkSettings),
  HealthAlerts(HealthAlertSettings),
  Holidays(HolidaySettings),
  CrashReports(CrashReportSettings),
}

/// Which installed apps may open this server's links (docs/app-links.md): what
/// `/.well-known/assetlinks.json` and `/.well-known/apple-app-site-association` say, on every brand's
/// host. The defaults are the official apps, so an instance using them sets nothing; another
/// publisher's build (other signing key, other Apple team) changes them in the console.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AppLinkSettings {
  /// Off: both files answer 404 and the apps leave links to the browser.
  pub is_enabled: bool,
  pub android_package_names: Vec<String>,
  /// SHA-256 of the signing certificates: upper-case hex pairs with colons.
  pub android_cert_fingerprints: Vec<String>,
  /// The Apple team of the iOS app; empty: no association file for Apple.
  pub apple_team_id: String,
  pub apple_bundle_id: String,
}

impl Default for AppLinkSettings {
  fn default() -> Self {
    Self {
      is_enabled: true,
      android_package_names: OFFICIAL_ANDROID_PACKAGES.map(str::to_owned).to_vec(),
      android_cert_fingerprints: vec![OFFICIAL_ANDROID_CERT_FINGERPRINT.to_owned()],
      apple_team_id: OFFICIAL_APPLE_TEAM_ID.to_owned(),
      apple_bundle_id: OFFICIAL_APP_ID.to_owned(),
    }
  }
}

/// Binding access tokens to devices with DPoP (e2ee.md §3.1.2, operations.md "Binding tokens to
/// devices"). The server's public origins are deployment facts, so they stay in the configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TokenBindingSettings {
  pub dpop_policy: DpopPolicy,
  /// How old a proof may be by the server's clock.
  pub proof_max_age_secs: u32,
  /// How far a client's clock may be off, on both ends of a proof's time window.
  pub proof_leeway_secs: u32,
  /// How often the server's nonce changes; the previous one stays good for as long.
  pub nonce_rotation_secs: u32,
  /// Most proofs remembered to refuse replays. When it is full and none has expired, requests with
  /// a bound token get 503, because accepting them could let a replay through.
  pub replay_cache_max_entries: u32,
  /// Most of those for one account, so that nobody fills it for everybody: past it, that account
  /// gets 429. Raise it for an account that imports a lot at once.
  #[serde(default = "default_dpop_replay_cache_max_entries_per_user")]
  pub replay_cache_max_entries_per_user: u32,
}

/// Default of [`TokenBindingSettings::proof_max_age_secs`]: a proof is made for one request.
pub const DEFAULT_DPOP_PROOF_MAX_AGE_SECS: u32 = 60;
/// Default of [`TokenBindingSettings::proof_leeway_secs`].
pub const DEFAULT_DPOP_PROOF_LEEWAY_SECS: u32 = 60;
/// Default of [`TokenBindingSettings::nonce_rotation_secs`]: at most one extra round trip per
/// client in this time, and a nonce is good for twice as long.
pub const DEFAULT_DPOP_NONCE_ROTATION_SECS: u32 = 300;
/// Default of [`TokenBindingSettings::replay_cache_max_entries`]: about 1100 requests per second
/// with the default window, at some 30 MiB when full.
pub const DEFAULT_DPOP_REPLAY_CACHE_MAX_ENTRIES: u32 = 200_000;
/// Default of [`TokenBindingSettings::replay_cache_max_entries_per_user`]: about 55 requests per
/// second with the default window, above what the per-address limit lets one device send, and 20
/// accounts must work together to fill the cache.
pub const DEFAULT_DPOP_REPLAY_CACHE_MAX_ENTRIES_PER_USER: u32 = 10_000;

fn default_dpop_replay_cache_max_entries_per_user() -> u32 {
  DEFAULT_DPOP_REPLAY_CACHE_MAX_ENTRIES_PER_USER
}

impl Default for TokenBindingSettings {
  fn default() -> Self {
    Self {
      dpop_policy: DpopPolicy::Optional,
      proof_max_age_secs: DEFAULT_DPOP_PROOF_MAX_AGE_SECS,
      proof_leeway_secs: DEFAULT_DPOP_PROOF_LEEWAY_SECS,
      nonce_rotation_secs: DEFAULT_DPOP_NONCE_ROTATION_SECS,
      replay_cache_max_entries: DEFAULT_DPOP_REPLAY_CACHE_MAX_ENTRIES,
      replay_cache_max_entries_per_user: DEFAULT_DPOP_REPLAY_CACHE_MAX_ENTRIES_PER_USER,
    }
  }
}

/// Rules only the server enforces (admin.md §6.3). Guest rules and password lengths are part of
/// the signed workspace policy instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ServerRulesSettings {
  /// Pending invites only apply to accounts whose provider verified the e-mail address.
  pub invites_require_verified_email: bool,
  /// Spaces every new member joins at the first sign-in.
  #[serde(default)]
  pub default_spaces: Vec<SpaceId>,
  /// Public channels of those spaces they join with them.
  #[serde(default)]
  pub default_channels: Vec<ConversationId>,
  /// Deactivated accounts are removed from all chats (devices removed, keys rotated) this many
  /// days after the deactivation; none (the default) keeps them until an admin removes them.
  #[serde(
    rename = "remove_deactivated_after_days",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_remove_deactivated_after_days: Option<u32>,
  /// Days an admin's request to join a private channel stays open for its owners (admin.md §2.4,
  /// `admin_self_join = owner_approval`). Absent from older servers: the default.
  #[serde(default = "default_admin_join_request_days")]
  pub admin_join_request_days: u32,
  /// A 1:1 direct message call ends for both as soon as one side leaves (issue #997); off, it keeps
  /// running until the last person leaves. Absent from older servers: on, the default.
  #[serde(default = "default_one_to_one_call_ends_for_both")]
  pub one_to_one_call_ends_for_both: bool,
}

/// How long an admin's request to join a private channel waits for an owner by default: a week
/// covers a holiday, and a stale request doesn't hang around for months.
pub const DEFAULT_ADMIN_JOIN_REQUEST_DAYS: u32 = 7;

fn default_admin_join_request_days() -> u32 {
  DEFAULT_ADMIN_JOIN_REQUEST_DAYS
}

fn default_one_to_one_call_ends_for_both() -> bool {
  true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct NotificationRuntimeSettings {
  /// People who never chose send read receipts in DMs and group DMs.
  pub read_receipts_default: bool,
  /// Which messages notify people who never chose a level: `all` or `mentions` (issue #1011).
  #[serde(default = "default_notification_level")]
  pub default_level: NotificationLevel,
  /// Urgent DMs are pushed again this often until acknowledged.
  pub urgent_repeat_interval_secs: u64,
  pub urgent_max_pushes: u32,
  /// While a desktop is in use, phone pushes wait this long (0: none while the desktop is attentive).
  pub mobile_push_delay_secs: u64,
  pub is_desktop_attention_enabled: bool,
  /// `@admins` mentions the instance's admins and owners (notifications.md, "Group mentions").
  /// Absent from older clients' requests: on.
  #[serde(default = "enabled")]
  pub is_admins_mention_enabled: bool,
}

/// Groups stored before the setting existed get the built-in default.
fn default_notification_level() -> NotificationLevel {
  NotificationLevel::All
}

/// The mail digest of missed mentions. Placeholders: `{count}` `{url}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EmailDigestSettings {
  pub is_enabled: bool,
  pub default_interval_secs: u64,
  pub min_interval_secs: u64,
  /// A mention counts as missed once it is this old and still unread.
  pub grace_secs: u64,
  pub subject_one: String,
  pub subject_many: String,
  pub body_one: String,
  pub body_many: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ShareSettings {
  pub is_enabled: bool,
  pub default_expiry_days: u32,
  pub max_expiry_days: u32,
  /// Most downloads a link may allow; 0 = no cap.
  pub max_downloads_cap: u32,
  pub max_links_per_user: u32,
}

/// Limits people notice. Protocol limits (message, MLS and key sizes) stay in the configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LimitSettings {
  /// Largest attachment (encrypted size).
  pub max_attachment_bytes: u64,
  pub max_attachment_bytes_per_user: u64,
  pub max_avatar_bytes: u64,
  /// Largest space or channel picture (the one limit for both) after the server re-encoded it. Absent from older clients' requests:
  /// the default applies.
  #[serde(default = "default_max_space_image_bytes")]
  pub max_space_image_bytes: u64,
  pub max_background_bytes: u64,
  pub max_backgrounds_per_user: u32,
  pub max_group_dm_participants: u32,
  pub max_user_group_members: u32,
  pub max_meeting_mails_per_user_per_hour: u32,
  /// Messages per hour and sender that name more people than `max_mentions_without_group_mentions`
  /// (large group mentions); 0 = no limit. Absent from older clients' requests: the default.
  #[serde(default = "default_max_large_mentions_per_user_per_hour")]
  pub max_large_mentions_per_user_per_hour: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct IntegrationSettings {
  /// Retention of integration channels in days; 0 = none.
  pub default_retention_days: u32,
  /// Where producers reach an integration bridge that runs on its own host (docs/bridge.md): the
  /// base of its `/hooks/` and `/ingest/` URLs. `https`, or `http` for loopback; no credentials,
  /// query or fragment ([`check_bridge_public_url`](crate::check_bridge_public_url)). Channel
  /// admins read it through `GET /api/v1/conversations/{id}/bridge-address`. Absent: none, and the
  /// apps offer the instance's own host name and the loopback default.
  #[serde(
    rename = "bridge_public_url",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_bridge_public_url: Option<String>,
}

/// Rules for local (password) accounts. They need `[local_accounts]` with a session secret in the
/// configuration; `is_enabled` switches them off and on at runtime (sign-in refused, sessions end).
/// The shortest password is part of the signed workspace policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LocalAccountSettings {
  #[serde(default = "enabled")]
  pub is_enabled: bool,
  pub require_totp: bool,
  pub activation_ttl_hours: u32,
  pub session_ttl_hours: u32,
  pub max_failed_attempts: u32,
  pub lockout_secs: u64,
}

/// How the sign-in page looks (`GET /api/v1/server-info`). The SSO button's provider name and
/// icon are brand settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SignInPageSettings {
  /// The methods offered, in this order; at least one, each once. `local` only shows while local
  /// accounts are available.
  pub methods: Vec<SignInMethod>,
  /// Language code -> help text below the form; `en` is the fallback.
  #[serde(default)]
  pub help_text: BTreeMap<String, String>,
  /// A help page (https).
  #[serde(rename = "help_link", default, skip_serializing_if = "Option::is_none")]
  pub maybe_help_link: Option<String>,
  /// Every sign-in shows the identity provider's login page (`prompt=login`), also with an
  /// existing provider session; off for operators who prefer silent single sign-on.
  #[serde(default = "enabled")]
  pub is_login_page_always_shown: bool,
}

fn enabled() -> bool {
  true
}

fn default_max_space_image_bytes() -> u64 {
  DEFAULT_MAX_SPACE_IMAGE_BYTES
}

fn default_max_large_mentions_per_user_per_hour() -> u32 {
  crate::DEFAULT_MAX_LARGE_MENTIONS_PER_USER_PER_HOUR
}

/// Texts of the server's own mail. Placeholders: invitation `{inviter}` `{space}` `{role}` `{url}`,
/// activation as documented in the configuration example.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MailTextSettings {
  pub invite_subject: String,
  pub invite_body: String,
  pub activation_subject: String,
  pub activation_body: String,
}

/// The regions of the bundled holiday table (ISO 3166-2): Germany's nationwide holidays and those
/// of each federal state, Austria's (all federal), Switzerland's federal ones and those of each
/// canton.
pub const HOLIDAY_REGIONS: [&str; 45] = [
  "DE", "DE-BW", "DE-BY", "DE-BE", "DE-BB", "DE-HB", "DE-HH", "DE-HE", "DE-MV", "DE-NI", "DE-NW", "DE-RP",
  "DE-SL", "DE-SN", "DE-ST", "DE-SH", "DE-TH", "AT", "CH", "CH-AG", "CH-AI", "CH-AR", "CH-BE", "CH-BL",
  "CH-BS", "CH-FR", "CH-GE", "CH-GL", "CH-GR", "CH-JU", "CH-LU", "CH-NE", "CH-NW", "CH-OW", "CH-SG", "CH-SH",
  "CH-SO", "CH-SZ", "CH-TG", "CH-TI", "CH-UR", "CH-VD", "CH-VS", "CH-ZG", "CH-ZH",
];

/// The default of [`HolidaySettings::feed_refresh_secs`]: once a day.
pub const DEFAULT_HOLIDAY_FEED_REFRESH_SECS: u64 = 86_400;
/// The default of [`HolidaySettings::feed_max_age_secs`]: a week.
pub const DEFAULT_HOLIDAY_FEED_MAX_AGE_SECS: u64 = 7 * 86_400;

/// The public holidays repeating meetings can skip (#1053): "Skip public holidays" in a series
/// leaves out its occurrences on these days, on the organizer's wall clock. The region's and the
/// feed's holidays count together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HolidaySettings {
  /// One of [`HOLIDAY_REGIONS`]; empty: no table.
  #[serde(default)]
  pub region: String,
  /// An iCalendar feed (https) whose all-day events are holidays, instead of the region or on top
  /// of it (#1426). The server fetches it in the background and keeps the last good copy.
  #[serde(rename = "feed_url", default, skip_serializing_if = "Option::is_none")]
  pub maybe_feed_url: Option<String>,
  /// How often the feed is fetched again.
  #[serde(default = "default_holiday_feed_refresh_secs")]
  pub feed_refresh_secs: u64,
  /// A copy older than this raises the health check `holiday_feed`.
  #[serde(default = "default_holiday_feed_max_age_secs")]
  pub feed_max_age_secs: u64,
}

impl Default for HolidaySettings {
  fn default() -> Self {
    Self {
      region: String::new(),
      maybe_feed_url: None,
      feed_refresh_secs: DEFAULT_HOLIDAY_FEED_REFRESH_SECS,
      feed_max_age_secs: DEFAULT_HOLIDAY_FEED_MAX_AGE_SECS,
    }
  }
}

fn default_holiday_feed_refresh_secs() -> u64 {
  DEFAULT_HOLIDAY_FEED_REFRESH_SECS
}

fn default_holiday_feed_max_age_secs() -> u64 {
  DEFAULT_HOLIDAY_FEED_MAX_AGE_SECS
}

wire_enum! {
  /// Why the last fetch of the holiday feed failed. The server keeps answering from its last good
  /// copy meanwhile.
  pub enum HolidayFeedError {
    /// No connection: name, address refused (private), TLS, or the connection broke.
    Unreachable => "unreachable",
    /// A phase took too long: connecting, the answer, or reading it.
    Timeout => "timeout",
    /// The server answered with an error status or a redirect it may not follow.
    HttpStatus => "http_status",
    /// Larger than the server reads.
    TooLarge => "too_large",
    /// Not a calendar: an HTML page (a captive portal, a login page) or something else.
    NotACalendar => "not_a_calendar",
    /// A calendar the server cannot read: cut off or broken.
    Malformed => "malformed",
    /// A calendar without any all-day event.
    Empty => "empty",
    /// More holidays than the server keeps.
    TooMany => "too_many",
  }
}

/// How the holiday feed stands, for the console; only while a feed is set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HolidayFeedStatus {
  /// When the copy in use was fetched; absent: none yet, nothing from the feed is skipped.
  #[serde(rename = "fetched_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_fetched_at: Option<UnixMillis>,
  /// Holidays in that copy.
  pub holidays: u32,
  /// The last attempt, successful or not.
  #[serde(rename = "last_attempt_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_attempt_at: Option<UnixMillis>,
  /// Why the last attempt failed; absent when it worked.
  #[serde(rename = "error", default, skip_serializing_if = "Option::is_none")]
  pub maybe_error: Option<HolidayFeedError>,
  /// The status code of an `http_status` error.
  #[serde(rename = "http_status", default, skip_serializing_if = "Option::is_none")]
  pub maybe_http_status: Option<u16>,
}

/// Meeting invitations. Placeholders: `{title}` `{organizer}` `{start}` `{end}` `{description}`
/// `{join_url}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MeetingMailSettings {
  pub max_invitees: u32,
  pub invitation_subject: String,
  pub invitation_body: String,
  pub update_subject: String,
  pub update_body: String,
  pub cancellation_subject: String,
  pub cancellation_body: String,
}

impl WriteOnlySecret {
  pub fn new(secret: String) -> Self {
    Self(secret)
  }

  /// The secret, for sealing it. Never log it.
  pub fn expose_secret(&self) -> &str {
    &self.0
  }
}

impl fmt::Debug for WriteOnlySecret {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("[redacted]")
  }
}

impl RuntimeSettingsSection {
  /// The group this section belongs to.
  pub fn key(&self) -> RuntimeSettingsKey {
    match self {
      Self::ServerRules(_) => RuntimeSettingsKey::ServerRules,
      Self::Notifications(_) => RuntimeSettingsKey::Notifications,
      Self::EmailDigest(_) => RuntimeSettingsKey::EmailDigest,
      Self::Shares(_) => RuntimeSettingsKey::Shares,
      Self::Limits(_) => RuntimeSettingsKey::Limits,
      Self::Integration(_) => RuntimeSettingsKey::Integration,
      Self::LocalAccounts(_) => RuntimeSettingsKey::LocalAccounts,
      Self::SignInPage(_) => RuntimeSettingsKey::SignInPage,
      Self::MailTexts(_) => RuntimeSettingsKey::MailTexts,
      Self::MeetingMail(_) => RuntimeSettingsKey::MeetingMail,
      Self::TokenBinding(_) => RuntimeSettingsKey::TokenBinding,
      Self::AppLinks(_) => RuntimeSettingsKey::AppLinks,
      Self::HealthAlerts(_) => RuntimeSettingsKey::HealthAlerts,
      Self::Holidays(_) => RuntimeSettingsKey::Holidays,
      Self::CrashReports(_) => RuntimeSettingsKey::CrashReports,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn sections_are_tagged_with_their_name() {
    let section = RuntimeSettingsSection::Integration(IntegrationSettings {
      default_retention_days: 30,
      maybe_bridge_public_url: None,
    });
    assert_eq!(
      serde_json::to_string(&section).unwrap(),
      r#"{"section":"integration","values":{"default_retention_days":30}}"#
    );
    assert_eq!(section.key(), RuntimeSettingsKey::Integration);
    assert!(
      serde_json::from_str::<RuntimeSettingsSection>(r#"{"section":"telephony","values":{"x":1}}"#).is_err()
    );
  }

  #[test]
  fn secret_updates_have_a_stable_shape_and_never_print_the_secret() {
    let set: SecretUpdate = serde_json::from_str(r#"{"action":"set","value":"hunter2"}"#).unwrap();
    assert_eq!(set, SecretUpdate::Set(WriteOnlySecret::new("hunter2".into())));
    assert!(!format!("{set:?}").contains("hunter2"));
    assert_eq!(
      serde_json::to_string(&SecretUpdate::Keep).unwrap(),
      r#"{"action":"keep"}"#
    );
    assert_eq!(
      serde_json::from_str::<SecretUpdate>(r#"{"action":"clear"}"#).unwrap(),
      SecretUpdate::Clear
    );
  }

  #[test]
  fn a_brand_mail_server_update_without_a_password_keeps_it() {
    let request: UpdateBrandMailServerRequest =
      serde_json::from_str(r#"{"host":"smtp.a.example.com","port":587,"security":"start_tls"}"#).unwrap();
    assert_eq!(request.password, SecretUpdate::Keep);
  }

  #[test]
  fn sources_and_modes_keep_their_wire_names() {
    assert_eq!(SettingSource::Database.as_str(), "database");
    assert_eq!(BrandSource::Builtin.as_str(), "builtin");
    assert_eq!(SmtpSecurityMode::Plain.as_str(), "none");
    assert_eq!(
      SmtpSecurityMode::from("start_tls".to_owned()),
      SmtpSecurityMode::StartTls
    );
    assert_eq!(
      RuntimeSettingsKey::from("meeting_mail".to_owned()),
      RuntimeSettingsKey::MeetingMail
    );
  }

  #[test]
  fn a_brand_without_optional_parts_parses() {
    let settings: BrandSettings = serde_json::from_str(r#"{"display_name":"Brand A"}"#).unwrap();
    assert_eq!(settings.display_name, "Brand A");
    assert!(settings.hostnames.is_empty() && settings.mail == BrandMailSettings::default());
  }
}
