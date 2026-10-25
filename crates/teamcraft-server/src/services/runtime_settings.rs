//! Instance settings admins change in the console (admin.md §6.3): groups of values that used to
//! need the configuration file. A group without a stored row comes from the file; storing one puts
//! it on top, deleting it goes back to the file.

use serde_json::json;
use teamcraft_db::StoredSetting;
use teamcraft_proto::{
  EmailDigestSettings, IntegrationSettings, LimitSettings, LocalAccountSettings, MailTextSettings,
  MeetingMailSettings, NotificationRuntimeSettings, Permission, RuntimeSettingsKey, RuntimeSettingsResponse,
  RuntimeSettingsSection, RuntimeSettingsState, ServerRulesSettings, SettingSource, ShareSettings,
  SignInMethod, SignInPageSettings, Target,
};
use tracing::warn;

use super::access::require;
use super::audit;
use super::convert::unix_millis;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::{AppState, Settings};

/// Prefix of the rows in `server_settings`.
const NAME_PREFIX: &str = "settings.";
const ALL_KEYS: [RuntimeSettingsKey; 10] = [
  RuntimeSettingsKey::ServerRules,
  RuntimeSettingsKey::Notifications,
  RuntimeSettingsKey::EmailDigest,
  RuntimeSettingsKey::Shares,
  RuntimeSettingsKey::Limits,
  RuntimeSettingsKey::Integration,
  RuntimeSettingsKey::LocalAccounts,
  RuntimeSettingsKey::SignInPage,
  RuntimeSettingsKey::MailTexts,
  RuntimeSettingsKey::MeetingMail,
];
const MAX_TEXT_CHARS: usize = 5000;
/// Longest help text of the sign-in page, per language.
const MAX_HELP_TEXT_CHARS: usize = 1000;
const MAX_LIST_ENTRIES: usize = 200;
/// Longest repeat or grace interval an admin can set: a day.
const MAX_INTERVAL_SECS: u64 = 86_400;

/// Puts the stored groups on top of the configuration; at startup and after every change.
pub async fn load(state: &AppState) -> Result<(), ApiError> {
  let stored = stored_sections(state).await?;
  let mut settings = (*state.settings.base()).clone();
  for (section, _) in &stored {
    apply(&mut settings, section);
  }
  state.settings.set_current(settings);
  Ok(())
}

/// The settings in force and where each group comes from.
pub async fn get_settings(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<RuntimeSettingsResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  response(state).await
}

/// Replaces one group.
pub async fn put_settings(
  state: &AppState,
  caller: &AuthenticatedUser,
  section: RuntimeSettingsSection,
) -> Result<RuntimeSettingsResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  validate(&section)?;
  let key = section.key();
  let value = serde_json::to_string(&section).map_err(|error| ApiError::Internal(error.to_string()))?;
  {
    let _guard = state.settings_lock.lock().await;
    state
      .database
      .put_server_setting(&row_name(&key), &value, None, caller.user.id)
      .await?;
    load(state).await?;
  }
  audit::record(
    state,
    Some(caller.user.id),
    "settings_updated",
    None,
    &json!({"section": key.as_str()}),
  )
  .await?;
  response(state).await
}

/// Goes back to the configuration file's values of one group.
pub async fn reset_settings(
  state: &AppState,
  caller: &AuthenticatedUser,
  key: RuntimeSettingsKey,
) -> Result<RuntimeSettingsResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  if matches!(key, RuntimeSettingsKey::Unknown(_)) {
    return Err(ApiError::InvalidRequest("unknown settings section".into()));
  }
  let is_deleted = {
    let _guard = state.settings_lock.lock().await;
    let is_deleted = state.database.delete_server_setting(&row_name(&key)).await?;
    load(state).await?;
    is_deleted
  };
  if is_deleted {
    audit::record(
      state,
      Some(caller.user.id),
      "settings_reset",
      None,
      &json!({"section": key.as_str()}),
    )
    .await?;
  }
  response(state).await
}

async fn response(state: &AppState) -> Result<RuntimeSettingsResponse, ApiError> {
  let stored = stored_sections(state).await?;
  let settings = state.settings.get();
  let sources = ALL_KEYS
    .iter()
    .map(|key| {
      let maybe_row = stored.iter().find(|(section, _)| section.key() == *key);
      RuntimeSettingsState {
        key: key.clone(),
        source: if maybe_row.is_some() {
          SettingSource::Database
        } else {
          SettingSource::Config
        },
        maybe_updated_at: maybe_row.map(|(_, row)| unix_millis(row.updated_at)),
      }
    })
    .collect();
  Ok(view(&settings, sources))
}

/// The stored groups that still parse; a row this version cannot read is skipped with a warning.
async fn stored_sections(state: &AppState) -> Result<Vec<(RuntimeSettingsSection, StoredSetting)>, ApiError> {
  let rows = state.database.server_settings().await?;
  Ok(
    rows
      .into_iter()
      .filter(|row| row.name.starts_with(NAME_PREFIX))
      .filter_map(
        |row| match serde_json::from_str::<RuntimeSettingsSection>(&row.value) {
          Ok(section) => Some((section, row)),
          Err(error) => {
            warn!(name = %row.name, %error, "ignoring stored settings this version cannot read");
            None
          },
        },
      )
      .collect(),
  )
}

fn row_name(key: &RuntimeSettingsKey) -> String {
  format!("{NAME_PREFIX}{}", key.as_str())
}

/// Puts one group's values into the settings.
fn apply(settings: &mut Settings, section: &RuntimeSettingsSection) {
  match section {
    RuntimeSettingsSection::ServerRules(rules) => {
      settings.policy.invites_require_verified_email = rules.invites_require_verified_email;
      settings.policy.default_spaces.clone_from(&rules.default_spaces);
      settings
        .policy
        .default_channels
        .clone_from(&rules.default_channels);
    },
    RuntimeSettingsSection::Notifications(values) => {
      let notifications = &mut settings.notifications;
      notifications.read_receipts_default = values.read_receipts_default;
      notifications.urgent_repeat_interval_secs = values.urgent_repeat_interval_secs;
      notifications.urgent_max_pushes = values.urgent_max_pushes;
      notifications.mobile_push_delay_secs = values.mobile_push_delay_secs;
      notifications.is_desktop_attention_enabled = values.is_desktop_attention_enabled;
    },
    RuntimeSettingsSection::EmailDigest(values) => {
      let digest = &mut settings.notifications.email_digest;
      digest.is_enabled = values.is_enabled;
      digest.default_interval_secs = values.default_interval_secs;
      digest.min_interval_secs = values.min_interval_secs;
      digest.grace_secs = values.grace_secs;
      digest.subject_one.clone_from(&values.subject_one);
      digest.subject_many.clone_from(&values.subject_many);
      digest.body_one.clone_from(&values.body_one);
      digest.body_many.clone_from(&values.body_many);
    },
    RuntimeSettingsSection::Shares(values) => {
      let shares = &mut settings.shares;
      shares.is_enabled = values.is_enabled;
      shares.default_expiry_days = values.default_expiry_days;
      shares.max_expiry_days = values.max_expiry_days;
      shares.max_downloads_cap = values.max_downloads_cap;
      shares.max_links_per_user = to_usize(values.max_links_per_user);
    },
    RuntimeSettingsSection::Limits(values) => {
      settings.max_attachment_bytes = values.max_attachment_bytes;
      let limits = &mut settings.limits;
      limits.max_attachment_bytes_per_user = values.max_attachment_bytes_per_user;
      limits.max_avatar_bytes = values.max_avatar_bytes;
      limits.max_space_image_bytes = values.max_space_image_bytes;
      limits.max_background_bytes = values.max_background_bytes;
      limits.max_backgrounds_per_user = values.max_backgrounds_per_user;
      limits.max_group_dm_participants = to_usize(values.max_group_dm_participants);
      limits.max_user_group_members = to_usize(values.max_user_group_members);
      limits.max_meeting_mails_per_user_per_hour = values.max_meeting_mails_per_user_per_hour;
    },
    RuntimeSettingsSection::Integration(values) => {
      settings.integration.default_retention_days = values.default_retention_days;
    },
    RuntimeSettingsSection::LocalAccounts(values) => {
      let local = &mut settings.local_accounts;
      // Turns local accounts off or on again; on needs them configured (session secret) at start.
      local.is_enabled = values.is_enabled;
      local.require_totp = values.require_totp;
      local.activation_ttl_hours = values.activation_ttl_hours;
      local.session_ttl_hours = values.session_ttl_hours;
      local.max_failed_attempts = values.max_failed_attempts;
      local.lockout_secs = values.lockout_secs;
    },
    RuntimeSettingsSection::SignInPage(values) => {
      let page = &mut settings.sign_in_page;
      page.methods.clone_from(&values.methods);
      page.help_text.clone_from(&values.help_text);
      page.maybe_help_link.clone_from(&values.maybe_help_link);
      page.is_login_page_always_shown = values.is_login_page_always_shown;
    },
    RuntimeSettingsSection::MailTexts(values) => {
      let mail = &mut settings.mail;
      mail.invite_subject.clone_from(&values.invite_subject);
      mail.invite_body.clone_from(&values.invite_body);
      mail.activation_subject.clone_from(&values.activation_subject);
      mail.activation_body.clone_from(&values.activation_body);
    },
    RuntimeSettingsSection::MeetingMail(values) => {
      let meeting = &mut settings.meeting_mail;
      meeting.max_invitees = to_usize(values.max_invitees);
      meeting
        .texts
        .invitation_subject
        .clone_from(&values.invitation_subject);
      meeting.texts.invitation_body.clone_from(&values.invitation_body);
      meeting.texts.update_subject.clone_from(&values.update_subject);
      meeting.texts.update_body.clone_from(&values.update_body);
      meeting
        .texts
        .cancellation_subject
        .clone_from(&values.cancellation_subject);
      meeting
        .texts
        .cancellation_body
        .clone_from(&values.cancellation_body);
    },
  }
}

/// The groups as the console shows them.
fn view(settings: &Settings, sources: Vec<RuntimeSettingsState>) -> RuntimeSettingsResponse {
  let notifications = &settings.notifications;
  let digest = &notifications.email_digest;
  let limits = &settings.limits;
  let local = &settings.local_accounts;
  let texts = &settings.meeting_mail.texts;
  RuntimeSettingsResponse {
    server_rules: ServerRulesSettings {
      invites_require_verified_email: settings.policy.invites_require_verified_email,
      default_spaces: settings.policy.default_spaces.clone(),
      default_channels: settings.policy.default_channels.clone(),
    },
    notifications: NotificationRuntimeSettings {
      read_receipts_default: notifications.read_receipts_default,
      urgent_repeat_interval_secs: notifications.urgent_repeat_interval_secs,
      urgent_max_pushes: notifications.urgent_max_pushes,
      mobile_push_delay_secs: notifications.mobile_push_delay_secs,
      is_desktop_attention_enabled: notifications.is_desktop_attention_enabled,
    },
    email_digest: EmailDigestSettings {
      is_enabled: digest.is_enabled,
      default_interval_secs: digest.default_interval_secs,
      min_interval_secs: digest.min_interval_secs,
      grace_secs: digest.grace_secs,
      subject_one: digest.subject_one.clone(),
      subject_many: digest.subject_many.clone(),
      body_one: digest.body_one.clone(),
      body_many: digest.body_many.clone(),
    },
    shares: ShareSettings {
      is_enabled: settings.shares.is_enabled,
      default_expiry_days: settings.shares.default_expiry_days,
      max_expiry_days: settings.shares.max_expiry_days,
      max_downloads_cap: settings.shares.max_downloads_cap,
      max_links_per_user: to_u32(settings.shares.max_links_per_user),
    },
    limits: LimitSettings {
      max_attachment_bytes: settings.max_attachment_bytes,
      max_attachment_bytes_per_user: limits.max_attachment_bytes_per_user,
      max_avatar_bytes: limits.max_avatar_bytes,
      max_space_image_bytes: limits.max_space_image_bytes,
      max_background_bytes: limits.max_background_bytes,
      max_backgrounds_per_user: limits.max_backgrounds_per_user,
      max_group_dm_participants: to_u32(limits.max_group_dm_participants),
      max_user_group_members: to_u32(limits.max_user_group_members),
      max_meeting_mails_per_user_per_hour: limits.max_meeting_mails_per_user_per_hour,
    },
    integration: IntegrationSettings {
      default_retention_days: settings.integration.default_retention_days,
    },
    local_accounts: LocalAccountSettings {
      is_enabled: local.is_enabled,
      require_totp: local.require_totp,
      activation_ttl_hours: local.activation_ttl_hours,
      session_ttl_hours: local.session_ttl_hours,
      max_failed_attempts: local.max_failed_attempts,
      lockout_secs: local.lockout_secs,
    },
    sign_in_page: SignInPageSettings {
      methods: settings.sign_in_page.methods.clone(),
      help_text: settings.sign_in_page.help_text.clone(),
      maybe_help_link: settings.sign_in_page.maybe_help_link.clone(),
      is_login_page_always_shown: settings.sign_in_page.is_login_page_always_shown,
    },
    mail_texts: MailTextSettings {
      invite_subject: settings.mail.invite_subject.clone(),
      invite_body: settings.mail.invite_body.clone(),
      activation_subject: settings.mail.activation_subject.clone(),
      activation_body: settings.mail.activation_body.clone(),
    },
    meeting_mail: MeetingMailSettings {
      max_invitees: to_u32(settings.meeting_mail.max_invitees),
      invitation_subject: texts.invitation_subject.clone(),
      invitation_body: texts.invitation_body.clone(),
      update_subject: texts.update_subject.clone(),
      update_body: texts.update_body.clone(),
      cancellation_subject: texts.cancellation_subject.clone(),
      cancellation_body: texts.cancellation_body.clone(),
    },
    sources,
  }
}

/// Rejects values that would break the server or its users; the bounds are generous on purpose.
fn validate(section: &RuntimeSettingsSection) -> Result<(), ApiError> {
  match section {
    RuntimeSettingsSection::ServerRules(rules) => {
      check_count("default_spaces", rules.default_spaces.len())?;
      check_count("default_channels", rules.default_channels.len())
    },
    RuntimeSettingsSection::Notifications(values) => {
      check_range(
        "urgent_repeat_interval_secs",
        values.urgent_repeat_interval_secs,
        30,
        MAX_INTERVAL_SECS,
      )?;
      check_range("urgent_max_pushes", u64::from(values.urgent_max_pushes), 1, 100)?;
      check_range("mobile_push_delay_secs", values.mobile_push_delay_secs, 0, 3600)
    },
    RuntimeSettingsSection::EmailDigest(values) => {
      check_range(
        "min_interval_secs",
        values.min_interval_secs,
        60,
        7 * MAX_INTERVAL_SECS,
      )?;
      check_range(
        "default_interval_secs",
        values.default_interval_secs,
        values.min_interval_secs,
        7 * MAX_INTERVAL_SECS,
      )?;
      check_range("grace_secs", values.grace_secs, 0, MAX_INTERVAL_SECS)?;
      for text in [&values.subject_one, &values.subject_many] {
        check_subject(text)?;
      }
      for text in [&values.body_one, &values.body_many] {
        check_body(text)?;
      }
      Ok(())
    },
    RuntimeSettingsSection::Shares(values) => {
      check_range("max_expiry_days", u64::from(values.max_expiry_days), 1, 3650)?;
      check_range(
        "default_expiry_days",
        u64::from(values.default_expiry_days),
        1,
        u64::from(values.max_expiry_days),
      )?;
      check_range(
        "max_links_per_user",
        u64::from(values.max_links_per_user),
        1,
        100_000,
      )
    },
    RuntimeSettingsSection::Limits(values) => {
      check_range("max_attachment_bytes", values.max_attachment_bytes, 1024, 1 << 40)?;
      check_range(
        "max_attachment_bytes_per_user",
        values.max_attachment_bytes_per_user,
        values.max_attachment_bytes,
        1 << 50,
      )?;
      check_range("max_avatar_bytes", values.max_avatar_bytes, 1024, 64 << 20)?;
      check_range(
        "max_space_image_bytes",
        values.max_space_image_bytes,
        64 * 1024,
        16 << 20,
      )?;
      check_range(
        "max_background_bytes",
        values.max_background_bytes,
        1024,
        64 << 20,
      )?;
      check_range(
        "max_backgrounds_per_user",
        u64::from(values.max_backgrounds_per_user),
        0,
        1000,
      )?;
      check_range(
        "max_group_dm_participants",
        u64::from(values.max_group_dm_participants),
        3,
        100,
      )?;
      check_range(
        "max_user_group_members",
        u64::from(values.max_user_group_members),
        1,
        100_000,
      )?;
      check_range(
        "max_meeting_mails_per_user_per_hour",
        u64::from(values.max_meeting_mails_per_user_per_hour),
        0,
        100_000,
      )
    },
    RuntimeSettingsSection::Integration(values) => check_range(
      "default_retention_days",
      u64::from(values.default_retention_days),
      0,
      36_500,
    ),
    RuntimeSettingsSection::LocalAccounts(values) => {
      check_range(
        "activation_ttl_hours",
        u64::from(values.activation_ttl_hours),
        1,
        24 * 90,
      )?;
      check_range(
        "session_ttl_hours",
        u64::from(values.session_ttl_hours),
        1,
        24 * 365,
      )?;
      check_range(
        "max_failed_attempts",
        u64::from(values.max_failed_attempts),
        1,
        1000,
      )?;
      check_range("lockout_secs", values.lockout_secs, 0, MAX_INTERVAL_SECS)
    },
    RuntimeSettingsSection::SignInPage(values) => check_sign_in_page(values),
    RuntimeSettingsSection::MailTexts(values) => {
      check_subject(&values.invite_subject)?;
      check_subject(&values.activation_subject)?;
      check_body(&values.invite_body)?;
      check_body(&values.activation_body)
    },
    RuntimeSettingsSection::MeetingMail(values) => {
      check_range("max_invitees", u64::from(values.max_invitees), 1, 10_000)?;
      for subject in [
        &values.invitation_subject,
        &values.update_subject,
        &values.cancellation_subject,
      ] {
        check_subject(subject)?;
      }
      for body in [
        &values.invitation_body,
        &values.update_body,
        &values.cancellation_body,
      ] {
        check_body(body)?;
      }
      Ok(())
    },
  }
}

/// At least one known method, each once; short help texts; an https help link.
fn check_sign_in_page(values: &SignInPageSettings) -> Result<(), ApiError> {
  let invalid = |reason: &str| Err(ApiError::InvalidRequest(reason.to_owned()));
  if values.methods.is_empty() {
    return invalid("choose at least one sign-in method");
  }
  for (index, method) in values.methods.iter().enumerate() {
    if matches!(method, SignInMethod::Unknown(_)) {
      return invalid("unknown sign-in method");
    }
    if values.methods[..index].contains(method) {
      return invalid("each sign-in method at most once");
    }
  }
  check_count("help_text", values.help_text.len())?;
  for (language, text) in &values.help_text {
    let is_language = !language.is_empty()
      && language.len() <= 16
      && language
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    if !is_language || text.chars().count() > MAX_HELP_TEXT_CHARS {
      return invalid("help texts need a language code and at most 1000 characters");
    }
  }
  if let Some(link) = &values.maybe_help_link {
    let is_https = url::Url::parse(link).is_ok_and(|url| {
      url.scheme() == "https" && url.host().is_some() && url.username().is_empty() && url.password().is_none()
    });
    if !is_https {
      return invalid("the help link must be an https address");
    }
  }
  Ok(())
}

fn check_range(name: &str, value: u64, min: u64, max: u64) -> Result<(), ApiError> {
  if (min..=max).contains(&value) {
    Ok(())
  } else {
    Err(ApiError::InvalidRequest(format!(
      "{name} must be between {min} and {max}"
    )))
  }
}

fn check_count(name: &str, count: usize) -> Result<(), ApiError> {
  if count <= MAX_LIST_ENTRIES {
    Ok(())
  } else {
    Err(ApiError::InvalidRequest(format!(
      "{name} has more than {MAX_LIST_ENTRIES} entries"
    )))
  }
}

/// A mail subject: one non-empty line (a line break would inject headers).
fn check_subject(text: &str) -> Result<(), ApiError> {
  if text.trim().is_empty() || text.chars().count() > 200 || text.chars().any(char::is_control) {
    return Err(ApiError::InvalidRequest(
      "mail subjects must be one non-empty line of at most 200 characters".into(),
    ));
  }
  Ok(())
}

fn check_body(text: &str) -> Result<(), ApiError> {
  if text.trim().is_empty()
    || text.chars().count() > MAX_TEXT_CHARS
    || text.chars().any(|c| c.is_control() && c != '\n' && c != '\t')
  {
    return Err(ApiError::InvalidRequest(format!(
      "mail texts must be non-empty and at most {MAX_TEXT_CHARS} characters"
    )));
  }
  Ok(())
}

fn to_usize(value: u32) -> usize {
  usize::try_from(value).unwrap_or(usize::MAX)
}

fn to_u32(value: usize) -> u32 {
  u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn every_group_round_trips_through_the_view() {
    let settings = Settings {
      max_attachment_bytes: 10 << 20,
      ..Settings::default()
    };
    let original = view(&settings, Vec::new());
    let sections = [
      RuntimeSettingsSection::ServerRules(original.server_rules.clone()),
      RuntimeSettingsSection::Notifications(original.notifications.clone()),
      RuntimeSettingsSection::EmailDigest(original.email_digest.clone()),
      RuntimeSettingsSection::Shares(original.shares.clone()),
      RuntimeSettingsSection::Limits(original.limits.clone()),
      RuntimeSettingsSection::Integration(original.integration.clone()),
      RuntimeSettingsSection::LocalAccounts(original.local_accounts.clone()),
      RuntimeSettingsSection::SignInPage(original.sign_in_page.clone()),
      RuntimeSettingsSection::MailTexts(original.mail_texts.clone()),
      RuntimeSettingsSection::MeetingMail(original.meeting_mail.clone()),
    ];
    assert_eq!(sections.len(), ALL_KEYS.len());
    let mut applied = Settings::default();
    for section in &sections {
      apply(&mut applied, section);
    }
    assert_eq!(view(&applied, Vec::new()), original);
  }

  #[test]
  fn a_stored_group_changes_only_its_own_values() {
    let mut settings = Settings::default();
    apply(
      &mut settings,
      &RuntimeSettingsSection::Integration(IntegrationSettings {
        default_retention_days: 7,
      }),
    );
    assert_eq!(settings.integration.default_retention_days, 7);
    assert_eq!(
      settings.notifications.read_receipts_default,
      Settings::default().notifications.read_receipts_default
    );
  }

  #[test]
  fn defaults_of_the_configuration_pass_validation() {
    let settings = Settings {
      max_attachment_bytes: 100 << 20,
      ..Settings::default()
    };
    let defaults = view(&settings, Vec::new());
    for section in [
      RuntimeSettingsSection::Notifications(defaults.notifications),
      RuntimeSettingsSection::EmailDigest(defaults.email_digest),
      RuntimeSettingsSection::Shares(defaults.shares),
      RuntimeSettingsSection::Limits(defaults.limits),
      RuntimeSettingsSection::Integration(defaults.integration),
      RuntimeSettingsSection::LocalAccounts(defaults.local_accounts),
      RuntimeSettingsSection::SignInPage(defaults.sign_in_page),
      RuntimeSettingsSection::MailTexts(defaults.mail_texts),
      RuntimeSettingsSection::MeetingMail(defaults.meeting_mail),
    ] {
      assert!(validate(&section).is_ok(), "{}", section.key());
    }
  }

  #[test]
  fn sign_in_pages_need_known_unique_methods_and_an_https_help_link() {
    let page = |methods: Vec<SignInMethod>, maybe_link: Option<&str>| {
      validate(&RuntimeSettingsSection::SignInPage(SignInPageSettings {
        methods,
        help_text: std::collections::BTreeMap::from([("en".to_owned(), "Call 1234.".to_owned())]),
        maybe_help_link: maybe_link.map(str::to_owned),
        is_login_page_always_shown: true,
      }))
    };
    assert!(
      page(
        vec![SignInMethod::Local, SignInMethod::Sso],
        Some("https://help.example.com")
      )
      .is_ok()
    );
    assert!(page(vec![SignInMethod::Local], None).is_ok());
    assert!(page(vec![], None).is_err());
    assert!(page(vec![SignInMethod::Sso, SignInMethod::Sso], None).is_err());
    assert!(page(vec![SignInMethod::Unknown("x".into())], None).is_err());
    assert!(page(vec![SignInMethod::Sso], Some("http://help.example.com")).is_err());
    assert!(page(vec![SignInMethod::Sso], Some("javascript:alert(1)")).is_err());
  }

  #[test]
  fn subjects_with_line_breaks_and_out_of_range_numbers_are_refused() {
    let mut texts = view(&Settings::default(), Vec::new()).mail_texts;
    texts.invite_subject = "Hi\r\nBcc: someone@example.com".into();
    assert!(validate(&RuntimeSettingsSection::MailTexts(texts)).is_err());
    let mut shares = view(&Settings::default(), Vec::new()).shares;
    shares.default_expiry_days = shares.max_expiry_days + 1;
    assert!(validate(&RuntimeSettingsSection::Shares(shares)).is_err());
  }
}
