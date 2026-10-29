//! Demo data for the runtime settings of the console (brands, mail, calendar, instance settings), so
//! the mock build works like the real one. Child module of `mock`.

use std::collections::BTreeMap;

use teamcraft_proto::{
  AdminBrand, AdminBrandsResponse, AdminCalendarSettingsResponse, AdminMailSettingsResponse, BrandAssetSlot,
  BrandColors, BrandLinks, BrandMailSettings, BrandSettings, BrandSource, CalDavSettings, CreateBrandRequest,
  EmailDigestSettings, IntegrationSettings, LimitSettings, LocalAccountSettings, MailDeliveryStatus,
  MailTextSettings, MeetingMailSettings, NotificationRuntimeSettings, RuntimeSettingsKey,
  RuntimeSettingsResponse, RuntimeSettingsSection, RuntimeSettingsState, SecretUpdate, ServedHostname,
  ServerRulesSettings, SettingSource, SettingsTestResponse, ShareSettings, SignInMethod, SignInPageSettings,
  SmtpSecurityMode, SmtpSettings, UnixMillis, UpdateBrandRequest, UpdateCalendarSettingsRequest,
  UpdateMailSettingsRequest,
};

use crate::admin::SettingsTestKind;

const MIB: u64 = 1024 * 1024;

/// Everything the mock keeps for the runtime settings.
pub(super) struct SettingsMock {
  pub(super) brands: AdminBrandsResponse,
  pub(super) mail: AdminMailSettingsResponse,
  pub(super) calendar: AdminCalendarSettingsResponse,
  pub(super) runtime: RuntimeSettingsResponse,
  pub(super) last_test: Option<(SettingsTestKind, SettingsTestResponse)>,
  pub(super) notetaker: teamcraft_proto::AdminNotetakerResponse,
}

impl SettingsMock {
  pub(super) fn new(now: i64) -> Self {
    let mut mock = Self {
      brands: AdminBrandsResponse {
        brands: vec![
          demo_brand(
            "brand-a",
            "Brand A",
            BrandSource::Config,
            true,
            &["chat.a.example.com"],
          ),
          demo_brand(
            "brand-b",
            "Brand B",
            BrandSource::Database,
            false,
            &["chat.b.example.com"],
          ),
        ],
        served_hostnames: ["chat.a.example.com", "chat.b.example.com", "team.b.example.com"]
          .into_iter()
          .map(|hostname| ServedHostname {
            hostname: hostname.to_owned(),
            maybe_brand_id: None,
          })
          .collect(),
        is_image_upload_available: true,
        max_asset_bytes: 512 * 1024,
      },
      mail: AdminMailSettingsResponse {
        source: SettingSource::Config,
        maybe_smtp: Some(SmtpSettings {
          host: "smtp.example.com".to_owned(),
          port: 587,
          security: SmtpSecurityMode::StartTls,
          maybe_username: Some("mailer".to_owned()),
          has_password: true,
          from_address: "teamcraft@example.com".to_owned(),
          maybe_from_name: Some("TeamCraft".to_owned()),
        }),
        can_store_secrets: true,
        status: MailDeliveryStatus {
          maybe_last_success_at: Some(UnixMillis(now - 3_600_000)),
          ..MailDeliveryStatus::default()
        },
      },
      calendar: AdminCalendarSettingsResponse {
        is_available: true,
        source: SettingSource::Config,
        maybe_caldav: Some(CalDavSettings {
          calendar_url: "https://calendar.example.com/dav/meetings/".to_owned(),
          username: "teamcraft".to_owned(),
          has_password: true,
          maybe_free_busy_url_template: None,
        }),
        can_store_secrets: true,
      },
      runtime: default_runtime(),
      last_test: None,
      notetaker: teamcraft_proto::AdminNotetakerResponse {
        is_key_stored: false,
        can_store_secrets: true,
        maybe_key_updated_at: None,
      },
    };
    mock.sync_hostnames();
    mock
  }

  pub(super) fn create_brand(&mut self, request: CreateBrandRequest) {
    if self.brands.brands.iter().any(|brand| brand.id == request.id) {
      return;
    }
    self.make_default_if(request.is_default, &request.id);
    self.brands.brands.push(AdminBrand {
      id: request.id,
      source: BrandSource::Database,
      is_default: request.is_default,
      is_overriding_config: false,
      settings: request.settings,
      assets: Vec::new(),
      maybe_updated_at: None,
    });
    self.sync_hostnames();
  }

  pub(super) fn update_brand(&mut self, id: &str, request: UpdateBrandRequest) {
    self.make_default_if(request.is_default, id);
    if let Some(brand) = self
      .brands
      .brands
      .iter_mut()
      .find(|brand| brand.id == id && brand.source == BrandSource::Database)
    {
      brand.settings = request.settings;
      brand.is_default = request.is_default;
    }
    self.sync_hostnames();
  }

  pub(super) fn delete_brand(&mut self, id: &str) {
    self
      .brands
      .brands
      .retain(|brand| !(brand.id == id && brand.source == BrandSource::Database && !brand.is_default));
    self.sync_hostnames();
  }

  pub(super) fn take_over_brand(&mut self, id: &str) {
    if let Some(brand) = self
      .brands
      .brands
      .iter_mut()
      .find(|brand| brand.id == id && brand.source == BrandSource::Config)
    {
      brand.source = BrandSource::Database;
      brand.is_overriding_config = true;
    }
  }

  pub(super) fn set_asset(&mut self, id: &str, slot: BrandAssetSlot, is_present: bool) {
    if let Some(brand) = self
      .brands
      .brands
      .iter_mut()
      .find(|brand| brand.id == id && brand.source == BrandSource::Database)
    {
      brand.assets.retain(|existing| *existing != slot);
      if is_present {
        brand.assets.push(slot);
      }
    }
  }

  pub(super) fn save_mail(&mut self, request: UpdateMailSettingsRequest) {
    let had_password = self
      .mail
      .maybe_smtp
      .as_ref()
      .is_some_and(|smtp| smtp.has_password);
    self.mail.source = SettingSource::Database;
    self.mail.maybe_smtp = Some(SmtpSettings {
      host: request.host,
      port: request.port,
      security: request.security,
      maybe_username: request.maybe_username,
      has_password: has_password_after(&request.password, had_password),
      from_address: request.from_address,
      maybe_from_name: request.maybe_from_name,
    });
  }

  pub(super) fn reset_mail(&mut self, now: i64) {
    let fresh = Self::new(now);
    self.mail = fresh.mail;
  }

  pub(super) fn save_calendar(&mut self, request: UpdateCalendarSettingsRequest) {
    let had_password = self
      .calendar
      .maybe_caldav
      .as_ref()
      .is_some_and(|caldav| caldav.has_password);
    self.calendar.source = SettingSource::Database;
    self.calendar.maybe_caldav = Some(CalDavSettings {
      calendar_url: request.calendar_url,
      username: request.username,
      has_password: has_password_after(&request.password, had_password),
      maybe_free_busy_url_template: request.maybe_free_busy_url_template,
    });
  }

  pub(super) fn reset_calendar(&mut self, now: i64) {
    let fresh = Self::new(now);
    self.calendar = fresh.calendar;
  }

  pub(super) fn save_runtime(&mut self, section: RuntimeSettingsSection) {
    let key = section.key();
    match section {
      RuntimeSettingsSection::ServerRules(values) => self.runtime.server_rules = values,
      RuntimeSettingsSection::Notifications(values) => self.runtime.notifications = values,
      RuntimeSettingsSection::EmailDigest(values) => self.runtime.email_digest = values,
      RuntimeSettingsSection::Shares(values) => self.runtime.shares = values,
      RuntimeSettingsSection::Limits(values) => self.runtime.limits = values,
      RuntimeSettingsSection::Integration(values) => self.runtime.integration = values,
      RuntimeSettingsSection::LocalAccounts(values) => self.runtime.local_accounts = values,
      RuntimeSettingsSection::SignInPage(values) => self.runtime.sign_in_page = values,
      RuntimeSettingsSection::MailTexts(values) => self.runtime.mail_texts = values,
      RuntimeSettingsSection::MeetingMail(values) => self.runtime.meeting_mail = values,
      // A section a newer wire version added; the mock has nothing to store.
      #[allow(unreachable_patterns)]
      _ => return,
    }
    self.set_source(key, SettingSource::Database);
  }

  pub(super) fn reset_runtime(&mut self, key: RuntimeSettingsKey) {
    let defaults = default_runtime();
    match key {
      RuntimeSettingsKey::ServerRules => self.runtime.server_rules = defaults.server_rules,
      RuntimeSettingsKey::Notifications => self.runtime.notifications = defaults.notifications,
      RuntimeSettingsKey::EmailDigest => self.runtime.email_digest = defaults.email_digest,
      RuntimeSettingsKey::Shares => self.runtime.shares = defaults.shares,
      RuntimeSettingsKey::Limits => self.runtime.limits = defaults.limits,
      RuntimeSettingsKey::Integration => self.runtime.integration = defaults.integration,
      RuntimeSettingsKey::LocalAccounts => self.runtime.local_accounts = defaults.local_accounts,
      RuntimeSettingsKey::SignInPage => self.runtime.sign_in_page = defaults.sign_in_page,
      RuntimeSettingsKey::MailTexts => self.runtime.mail_texts = defaults.mail_texts,
      RuntimeSettingsKey::MeetingMail => self.runtime.meeting_mail = defaults.meeting_mail,
      _ => return,
    }
    self.set_source(key, SettingSource::Config);
  }

  fn set_source(&mut self, key: RuntimeSettingsKey, source: SettingSource) {
    if let Some(state) = self.runtime.sources.iter_mut().find(|state| state.key == key) {
      state.source = source;
    }
  }

  fn make_default_if(&mut self, is_default: bool, id: &str) {
    if is_default {
      for brand in &mut self.brands.brands {
        brand.is_default = brand.id == id;
      }
    }
  }

  /// Each host name belongs to the first brand that lists it.
  fn sync_hostnames(&mut self) {
    for served in &mut self.brands.served_hostnames {
      served.maybe_brand_id = self
        .brands
        .brands
        .iter()
        .find(|brand| brand.settings.hostnames.contains(&served.hostname))
        .map(|brand| brand.id.clone());
    }
  }
}

/// Whether a stored password exists after the update.
fn has_password_after(update: &SecretUpdate, had_password: bool) -> bool {
  match update {
    SecretUpdate::Keep => had_password,
    SecretUpdate::Clear => false,
    SecretUpdate::Set(_) => true,
  }
}

fn demo_brand(id: &str, name: &str, source: BrandSource, is_default: bool, hosts: &[&str]) -> AdminBrand {
  AdminBrand {
    id: id.to_owned(),
    source,
    is_default,
    is_overriding_config: false,
    settings: BrandSettings {
      display_name: name.to_owned(),
      maybe_organization_name: Some(format!("{name} Ltd.")),
      hostnames: hosts.iter().map(|host| (*host).to_owned()).collect(),
      email_domains: vec![format!("{id}.example.com")],
      colors: BrandColors {
        light: BTreeMap::from([
          ("accent".to_owned(), "#0b7f78".to_owned()),
          ("on_accent".to_owned(), "#ffffff".to_owned()),
        ]),
        dark: BTreeMap::new(),
      },
      login_title: BTreeMap::from([("en".to_owned(), format!("Welcome to {name}"))]),
      login_text: BTreeMap::new(),
      links: BrandLinks {
        maybe_support: Some("https://support.example.com".to_owned()),
        ..BrandLinks::default()
      },
      mail: BrandMailSettings::default(),
      maybe_sso_label: None,
    },
    assets: Vec::new(),
    maybe_updated_at: None,
  }
}

fn default_runtime() -> RuntimeSettingsResponse {
  let texts = |subject: &str, body: &str| (subject.to_owned(), body.to_owned());
  let (invite_subject, invite_body) = texts(
    "{inviter} invited you to {space}",
    "Join {space} as {role}: {url}",
  );
  let (activation_subject, activation_body) = texts("Activate your account", "Activate your account: {url}");
  RuntimeSettingsResponse {
    server_rules: ServerRulesSettings {
      invites_require_verified_email: true,
      default_spaces: Vec::new(),
      default_channels: Vec::new(),
      maybe_remove_deactivated_after_days: None,
      admin_join_request_days: teamcraft_proto::DEFAULT_ADMIN_JOIN_REQUEST_DAYS,
    },
    notifications: NotificationRuntimeSettings {
      read_receipts_default: true,
      urgent_repeat_interval_secs: 120,
      urgent_max_pushes: 5,
      mobile_push_delay_secs: 30,
      is_desktop_attention_enabled: true,
    },
    email_digest: EmailDigestSettings {
      is_enabled: true,
      default_interval_secs: 3_600,
      min_interval_secs: 900,
      grace_secs: 600,
      subject_one: "You missed a mention".to_owned(),
      subject_many: "You missed {count} mentions".to_owned(),
      body_one: "Open {url} to read it.".to_owned(),
      body_many: "Open {url} to read {count} mentions.".to_owned(),
    },
    shares: ShareSettings {
      is_enabled: true,
      default_expiry_days: 7,
      max_expiry_days: 30,
      max_downloads_cap: 0,
      max_links_per_user: 50,
    },
    limits: LimitSettings {
      max_attachment_bytes: 100 * MIB,
      max_attachment_bytes_per_user: 2_048 * MIB,
      max_avatar_bytes: 2 * MIB,
      max_space_image_bytes: MIB,
      max_background_bytes: 5 * MIB,
      max_backgrounds_per_user: 5,
      max_group_dm_participants: 9,
      max_user_group_members: 200,
      max_meeting_mails_per_user_per_hour: 20,
    },
    integration: IntegrationSettings {
      default_retention_days: 30,
    },
    local_accounts: LocalAccountSettings {
      is_enabled: true,
      require_totp: false,
      activation_ttl_hours: 72,
      session_ttl_hours: 12,
      max_failed_attempts: 5,
      lockout_secs: 900,
    },
    sign_in_page: SignInPageSettings {
      methods: vec![SignInMethod::Sso, SignInMethod::Local],
      help_text: BTreeMap::new(),
      maybe_help_link: None,
      is_login_page_always_shown: true,
    },
    mail_texts: MailTextSettings {
      invite_subject,
      invite_body,
      activation_subject,
      activation_body,
    },
    meeting_mail: MeetingMailSettings {
      max_invitees: 50,
      invitation_subject: "Invitation: {title}".to_owned(),
      invitation_body: "{organizer} invites you to {title}, {start} to {end}. Join: {join_url}".to_owned(),
      update_subject: "Updated: {title}".to_owned(),
      update_body: "{title} changed: {start} to {end}. Join: {join_url}".to_owned(),
      cancellation_subject: "Canceled: {title}".to_owned(),
      cancellation_body: "{title} was canceled.".to_owned(),
    },
    sources: [
      RuntimeSettingsKey::ServerRules,
      RuntimeSettingsKey::Notifications,
      RuntimeSettingsKey::EmailDigest,
      RuntimeSettingsKey::Shares,
      RuntimeSettingsKey::Limits,
      RuntimeSettingsKey::Integration,
      RuntimeSettingsKey::LocalAccounts,
      RuntimeSettingsKey::MailTexts,
      RuntimeSettingsKey::MeetingMail,
    ]
    .into_iter()
    .map(|key| RuntimeSettingsState {
      key,
      source: SettingSource::Config,
      maybe_updated_at: None,
    })
    .collect(),
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::WriteOnlySecret;

  use super::*;

  #[test]
  fn host_names_follow_the_brands_that_list_them() {
    let mut mock = SettingsMock::new(0);
    let owner = |mock: &SettingsMock, host: &str| {
      mock
        .brands
        .served_hostnames
        .iter()
        .find(|served| served.hostname == host)
        .and_then(|served| served.maybe_brand_id.clone())
    };
    assert_eq!(owner(&mock, "chat.b.example.com").as_deref(), Some("brand-b"));
    assert_eq!(owner(&mock, "team.b.example.com"), None);
    let mut settings = mock.brands.brands[1].settings.clone();
    settings.hostnames.push("team.b.example.com".to_owned());
    mock.update_brand(
      "brand-b",
      UpdateBrandRequest {
        is_default: false,
        settings,
      },
    );
    assert_eq!(owner(&mock, "team.b.example.com").as_deref(), Some("brand-b"));
  }

  #[test]
  fn secrets_are_kept_cleared_or_set() {
    assert!(has_password_after(&SecretUpdate::Keep, true));
    assert!(!has_password_after(&SecretUpdate::Keep, false));
    assert!(!has_password_after(&SecretUpdate::Clear, true));
    assert!(has_password_after(
      &SecretUpdate::Set(WriteOnlySecret::new("x".into())),
      false
    ));
  }

  #[test]
  fn saving_and_resetting_a_group_changes_its_source() {
    let mut mock = SettingsMock::new(0);
    let mut shares = mock.runtime.shares.clone();
    shares.max_links_per_user = 3;
    mock.save_runtime(RuntimeSettingsSection::Shares(shares));
    let source = |mock: &SettingsMock| {
      mock
        .runtime
        .sources
        .iter()
        .find(|state| state.key == RuntimeSettingsKey::Shares)
        .map(|state| state.source.clone())
    };
    assert_eq!(source(&mock), Some(SettingSource::Database));
    mock.reset_runtime(RuntimeSettingsKey::Shares);
    assert_eq!(source(&mock), Some(SettingSource::Config));
    assert_eq!(mock.runtime.shares.max_links_per_user, 50);
  }
}
