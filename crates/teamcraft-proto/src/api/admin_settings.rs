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
use crate::{BrandAssetSlot, BrandColors, BrandLinks, ConversationId, SignInMethod, SpaceId, UnixMillis};

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "action", content = "value", rename_all = "snake_case")]
pub enum SecretUpdate {
  /// Leave the stored secret as it is.
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct NotificationRuntimeSettings {
  /// People who never chose send read receipts in DMs and group DMs.
  pub read_receipts_default: bool,
  /// Urgent DMs are pushed again this often until acknowledged.
  pub urgent_repeat_interval_secs: u64,
  pub urgent_max_pushes: u32,
  /// While a desktop is in use, phone pushes wait this long (0: none while the desktop is attentive).
  pub mobile_push_delay_secs: u64,
  pub is_desktop_attention_enabled: bool,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct IntegrationSettings {
  /// Retention of integration channels in days; 0 = none.
  pub default_retention_days: u32,
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

/// Texts of the server's own mail. Placeholders: invitation `{inviter}` `{space}` `{role}` `{url}`,
/// activation as documented in the configuration example.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MailTextSettings {
  pub invite_subject: String,
  pub invite_body: String,
  pub activation_subject: String,
  pub activation_body: String,
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
