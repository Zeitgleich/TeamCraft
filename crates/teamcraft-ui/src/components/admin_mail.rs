use leptos::prelude::*;
use teamcraft_proto::{
  SecretUpdate, SendTestMailRequest, SettingSource, SmtpSecurityMode, SmtpSettings,
  UpdateMailSettingsRequest, WriteOnlySecret,
};

use crate::admin::{AdminSection, SettingsTestKind};
use crate::clock::format_date_time;
use crate::components::admin_console::SectionHeader;
use crate::components::admin_fields::{
  CheckField, FieldErrors, SourceBadge, TextField, Validated, accept_or_refuse, bind, bind_bool, is_dns_name,
  is_email_address, parse_number,
};
use crate::components::admin_policy::SelectRow;
use crate::i18n::use_i18n;
use crate::state::use_app_state;

const FORM_ID: &str = "admin-mail-form";
const DEFAULT_PORT: u16 = 587;
const MAX_TEXT_CHARS: usize = 200;
const SECURITY_MODES: [(&str, &str); 3] = [
  ("start_tls", "admin-mail-security-start-tls"),
  ("tls", "admin-mail-security-tls"),
  ("none", "admin-mail-security-none"),
];

/// The mail server all of the server's own mail goes through (invitations, activations, digests,
/// meeting invitations): editable here, with the configuration file as the fallback.
#[component]
pub fn MailSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let current = Memo::new(move |_| state.read(|backend| backend.mail_settings()));
  let smtp = Memo::new(move |_| current.get().and_then(|mail| mail.maybe_smtp));
  let draft = RwSignal::new(MailDraft::from_settings(smtp.get_untracked().as_ref()));
  let show_errors = RwSignal::new(false);
  let test_to = RwSignal::new(String::new());
  let is_testing = RwSignal::new(false);
  // Follow the server when the stored settings change (after a save or a reset).
  Effect::new(move |previous: Option<()>| {
    let latest = smtp.get();
    if previous.is_some() {
      draft.set(MailDraft::from_settings(latest.as_ref()));
      show_errors.set(false);
    }
  });
  let can_store_secrets = Memo::new(move |_| current.get().is_some_and(|mail| mail.can_store_secrets));
  let source = Memo::new(move |_| current.get().map_or(SettingSource::Config, |mail| mail.source));
  let has_password = Memo::new(move |_| smtp.get().is_some_and(|smtp| smtp.has_password));
  let test_result = Memo::new(move |_| {
    state
      .read(|backend| backend.last_settings_test())
      .filter(|(kind, _)| *kind == SettingsTestKind::Mail)
      .map(|(_, response)| response)
  });
  Effect::new(move |_| {
    if test_result.get().is_some() {
      is_testing.set(false);
    }
  });

  let host = bind(draft, show_errors, "host", |d| &d.host, |d| &mut d.host);
  let port = bind(draft, show_errors, "port", |d| &d.port, |d| &mut d.port);
  let username = bind(
    draft,
    show_errors,
    "username",
    |d| &d.username,
    |d| &mut d.username,
  );
  let password = bind(
    draft,
    show_errors,
    "password",
    |d| &d.password,
    |d| &mut d.password,
  );
  let from_address = bind(
    draft,
    show_errors,
    "from_address",
    |d| &d.from_address,
    |d| &mut d.from_address,
  );
  let from_name = bind(
    draft,
    show_errors,
    "from_name",
    |d| &d.from_name,
    |d| &mut d.from_name,
  );
  let (is_clearing, set_clearing) = bind_bool(
    draft,
    |d| d.is_clearing_password,
    |d, v| d.is_clearing_password = v,
  );

  let security_options = Signal::derive(move || {
    SECURITY_MODES
      .into_iter()
      .map(|(slug, key)| (slug.to_owned(), i18n.t(key)))
      .collect()
  });
  let save = move |ev: web_sys::SubmitEvent| {
    ev.prevent_default();
    if !accept_or_refuse(draft, show_errors, FORM_ID) {
      return;
    }
    let request = draft.with_untracked(|draft| draft.to_request(can_store_secrets.get_untracked()));
    if let Some(request) = request {
      state.write(|backend| backend.save_mail_settings(request));
    }
  };
  let send_test = move |_| {
    let to = test_to.get_untracked();
    let to = to.trim();
    if !to.is_empty() && !is_email_address(to) {
      return;
    }
    is_testing.set(true);
    let request = SendTestMailRequest {
      maybe_to_address: (!to.is_empty()).then(|| to.to_owned()),
    };
    state.write(|backend| backend.send_test_mail(request));
  };
  let test_address_error = Signal::derive(move || {
    let to = test_to.get();
    let to = to.trim();
    (!to.is_empty() && !is_email_address(to)).then_some("admin-error-email")
  });

  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-mail-title"))
      subtitle=Signal::derive(move || i18n.t("admin-mail-subtitle"))
    />
    {move || {
      current
        .get()
        .is_none()
        .then(|| view! { <p class="muted-text" role="status">{i18n.t("admin-settings-loading")}</p> })
    }}
    <form id=FORM_ID class="policy-form" on:submit=save>
      <fieldset class="policy-group">
        <legend>{move || i18n.t("admin-mail-server")}</legend>
        <p class="settings-source small">
          <SourceBadge source=source />
          {move || {
            smtp.get().is_none().then(|| view! { <span class="muted-text">{i18n.t("admin-mail-none")}</span> })
          }}
        </p>
        <TextField
          id="mail-host"
          label="admin-mail-host"
          placeholder="smtp.example.com"
          value=host.value
          on_input=host.on_input
          error=host.error
        />
        <TextField
          id="mail-port"
          label="admin-mail-port"
          input_type="number"
          value=port.value
          on_input=port.on_input
          error=port.error
        />
        <SelectRow
          id="mail-security"
          label=Signal::derive(move || i18n.t("admin-mail-security"))
          options=security_options
          selected=Signal::derive(move || draft.with(|d| d.security.clone()))
          on_select=Callback::new(move |slug: String| draft.update(|d| d.security = slug))
        />
        <TextField
          id="mail-username"
          label="admin-mail-username"
          value=username.value
          on_input=username.on_input
          error=username.error
        />
        <TextField
          id="mail-password"
          label="admin-mail-password"
          input_type="password"
          hint="admin-mail-password-hint"
          value=password.value
          on_input=password.on_input
          error=password.error
          disabled=Signal::derive(move || !can_store_secrets.get())
        />
        {move || {
          (!can_store_secrets.get())
            .then(|| view! { <p class="auth-alert policy-blocked" role="status">{i18n.t("admin-secrets-unavailable")}</p> })
        }}
        {move || {
          (can_store_secrets.get() && has_password.get())
            .then(|| view! {
              <CheckField label="admin-mail-clear-password" checked=is_clearing on_change=set_clearing />
            })
        }}
        <TextField
          id="mail-from-address"
          label="admin-mail-from-address"
          placeholder="teamcraft@example.com"
          hint="admin-mail-from-address-hint"
          value=from_address.value
          on_input=from_address.on_input
          error=from_address.error
        />
        <TextField
          id="mail-from-name"
          label="admin-mail-from-name"
          value=from_name.value
          on_input=from_name.on_input
          error=from_name.error
        />
        <div class="policy-footer">
          <button type="submit" class="btn btn-primary">{move || i18n.t("admin-save")}</button>
          <button
            type="button"
            class="btn"
            prop:disabled=move || source.get() != SettingSource::Database
            on:click=move |_| state.write(|backend| backend.reset_mail_settings())
          >
            {move || i18n.t("admin-reset-to-config")}
          </button>
        </div>
      </fieldset>
    </form>
    <section class="policy-group policy-form" aria-label=move || i18n.t("admin-mail-status")>
      <h3>{move || i18n.t("admin-mail-status")}</h3>
      <MailStatus current=current />
      <p class="muted-text small">{move || i18n.t("admin-mail-test-hint")}</p>
      <TextField
        id="mail-test-to"
        label="admin-mail-test-to"
        input_type="email"
        value=test_to
        on_input=Callback::new(move |text: String| test_to.set(text))
        error=test_address_error
      />
      <button
        type="button"
        class="btn"
        prop:disabled=move || smtp.get().is_none() || is_testing.get()
        on:click=send_test
      >
        {move || i18n.t("admin-mail-test-send")}
      </button>
      <p class="muted-text small" role="status" aria-live="polite">
        {move || {
          if is_testing.get() {
            i18n.t("admin-test-running")
          } else {
            match test_result.get() {
              Some(response) if response.is_ok => i18n.t("admin-test-ok"),
              Some(response) => {
                i18n.t_with("admin-test-failed", &[("detail", &response.maybe_error.unwrap_or_default())])
              },
              None => String::new(),
            }
          }
        }}
      </p>
    </section>
    <BrandSenders />
  }
}

#[component]
fn MailStatus(current: Memo<Option<teamcraft_proto::AdminMailSettingsResponse>>) -> impl IntoView {
  let i18n = use_i18n();
  let time = move |maybe_at: Option<teamcraft_proto::UnixMillis>| match maybe_at {
    Some(at) => format_date_time(at.0, i18n.locale().tag()),
    None => i18n.t("admin-mail-never"),
  };
  view! {
    <dl class="settings-status">
      <dt>{move || i18n.t("admin-mail-last-success")}</dt>
      <dd>{move || time(current.get().and_then(|mail| mail.status.maybe_last_success_at))}</dd>
      <dt>{move || i18n.t("admin-mail-last-failure")}</dt>
      <dd>{move || time(current.get().and_then(|mail| mail.status.maybe_last_failure_at))}</dd>
      {move || {
        current
          .get()
          .and_then(|mail| mail.status.maybe_last_error)
          .map(|error| {
            view! {
              <dt>{i18n.t("admin-mail-last-error")}</dt>
              <dd translate="no">{error}</dd>
            }
          })
      }}
    </dl>
  }
}

/// Which sender each brand uses; edited in the brand itself.
#[component]
fn BrandSenders() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let brands = Memo::new(move |_| state.read(|backend| backend.admin_brands()));
  let default_from = Memo::new(move |_| {
    state
      .read(|backend| backend.mail_settings())
      .and_then(|mail| mail.maybe_smtp)
      .map(|smtp| smtp.from_address)
      .unwrap_or_default()
  });
  view! {
    <section class="policy-group policy-form" aria-label=move || i18n.t("admin-mail-senders")>
      <h3>{move || i18n.t("admin-mail-senders")}</h3>
      <p class="muted-text small">{move || i18n.t("admin-mail-senders-hint")}</p>
      <div class="table-wrap">
        <table class="data-table">
          <thead>
            <tr>
              <th>{move || i18n.t("admin-brands-name")}</th>
              <th>{move || i18n.t("admin-mail-from-address")}</th>
              <th>{move || i18n.t("admin-mail-from-name")}</th>
            </tr>
          </thead>
          <tbody>
            {move || {
              brands
                .get()
                .map(|response| response.brands)
                .unwrap_or_default()
                .into_iter()
                .map(|brand| {
                  let own = brand.settings.mail.maybe_from_address.clone();
                  let is_own = own.is_some();
                  view! {
                    <tr>
                      <td>{brand.settings.display_name.clone()}</td>
                      <td translate="no">
                        {own.unwrap_or_else(|| default_from.get_untracked())}
                        {(!is_own).then(|| view! { <span class="muted-text small">{" "}{i18n.t("admin-mail-sender-default")}</span> })}
                      </td>
                      <td>{brand.settings.mail.maybe_sender_name.clone().unwrap_or_default()}</td>
                    </tr>
                  }
                })
                .collect_view()
            }}
          </tbody>
        </table>
      </div>
      <button type="button" class="btn" on:click=move |_| state.open_admin(AdminSection::Brands)>
        {move || i18n.t("admin-mail-open-brands")}
      </button>
    </section>
  }
}

/// What the mail form holds: texts as typed, so a half-typed port is not lost.
#[derive(Clone, Debug, PartialEq, Eq)]
struct MailDraft {
  host: String,
  port: String,
  security: String,
  username: String,
  password: String,
  is_clearing_password: bool,
  from_address: String,
  from_name: String,
}

impl MailDraft {
  fn from_settings(maybe_smtp: Option<&SmtpSettings>) -> Self {
    match maybe_smtp {
      Some(smtp) => Self {
        host: smtp.host.clone(),
        port: smtp.port.to_string(),
        security: smtp.security.as_str().to_owned(),
        username: smtp.maybe_username.clone().unwrap_or_default(),
        password: String::new(),
        is_clearing_password: false,
        from_address: smtp.from_address.clone(),
        from_name: smtp.maybe_from_name.clone().unwrap_or_default(),
      },
      None => Self {
        host: String::new(),
        port: DEFAULT_PORT.to_string(),
        security: SmtpSecurityMode::StartTls.as_str().to_owned(),
        username: String::new(),
        password: String::new(),
        is_clearing_password: false,
        from_address: String::new(),
        from_name: String::new(),
      },
    }
  }

  /// `None` if the port is not a number; check [`Validated::errors`] first.
  fn to_request(&self, can_store_secrets: bool) -> Option<UpdateMailSettingsRequest> {
    let username = self.username.trim();
    let from_name = self.from_name.trim();
    Some(UpdateMailSettingsRequest {
      host: self.host.trim().to_owned(),
      port: parse_number::<u16>(&self.port)?,
      security: SmtpSecurityMode::from(self.security.clone()),
      maybe_username: (!username.is_empty()).then(|| username.to_owned()),
      password: secret_update(&self.password, self.is_clearing_password, can_store_secrets),
      from_address: self.from_address.trim().to_owned(),
      maybe_from_name: (!from_name.is_empty()).then(|| from_name.to_owned()),
    })
  }
}

impl Validated for MailDraft {
  fn errors(&self) -> FieldErrors {
    let mut errors = FieldErrors::new();
    let host = self.host.trim();
    if !is_plausible_host(host) {
      errors.push(("host", "admin-error-host"));
    }
    if parse_number::<u16>(&self.port).is_none_or(|port| port == 0) {
      errors.push(("port", "admin-error-port"));
    }
    if !is_email_address(self.from_address.trim()) {
      errors.push(("from_address", "admin-error-email"));
    }
    if self.username.chars().count() > MAX_TEXT_CHARS || self.from_name.chars().count() > MAX_TEXT_CHARS {
      errors.push(("from_name", "admin-error-too-long"));
    }
    errors
  }
}

/// What to do with a write-only password: a typed one replaces it, the checkbox removes it, and an
/// empty field keeps it. Without a key to store secrets nothing changes.
pub fn secret_update(typed: &str, is_clearing: bool, can_store_secrets: bool) -> SecretUpdate {
  if !can_store_secrets {
    SecretUpdate::Keep
  } else if !typed.is_empty() {
    SecretUpdate::Set(WriteOnlySecret::new(typed.to_owned()))
  } else if is_clearing {
    SecretUpdate::Clear
  } else {
    SecretUpdate::Keep
  }
}

/// Whether a mail host is a DNS name or an IP address.
fn is_plausible_host(host: &str) -> bool {
  is_dns_name(&host.to_ascii_lowercase()) || host.parse::<std::net::IpAddr>().is_ok()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn passwords_are_kept_set_or_cleared() {
    assert_eq!(secret_update("", false, true), SecretUpdate::Keep);
    assert_eq!(secret_update("", true, true), SecretUpdate::Clear);
    assert_eq!(
      secret_update("hunter2", true, true),
      SecretUpdate::Set(WriteOnlySecret::new("hunter2".into()))
    );
    assert_eq!(secret_update("hunter2", false, false), SecretUpdate::Keep);
    assert_eq!(secret_update("", true, false), SecretUpdate::Keep);
  }

  #[test]
  fn the_draft_round_trips_and_defaults_without_settings() {
    let fresh = MailDraft::from_settings(None);
    assert_eq!(
      (fresh.port.as_str(), fresh.security.as_str()),
      ("587", "start_tls")
    );
    let smtp = SmtpSettings {
      host: "smtp.example.com".into(),
      port: 465,
      security: SmtpSecurityMode::Tls,
      maybe_username: Some("mailer".into()),
      has_password: true,
      from_address: "noreply@example.com".into(),
      maybe_from_name: None,
    };
    let draft = MailDraft::from_settings(Some(&smtp));
    assert!(draft.errors().is_empty());
    let request = draft.to_request(true).unwrap();
    assert_eq!(request.port, 465);
    assert_eq!(request.security, SmtpSecurityMode::Tls);
    assert_eq!(request.password, SecretUpdate::Keep);
    assert_eq!(request.maybe_from_name, None);
  }

  #[test]
  fn obvious_mistakes_are_reported_per_field() {
    let mut draft = MailDraft::from_settings(None);
    draft.host = "https://smtp.example.com".into();
    draft.port = "70000".into();
    draft.from_address = "nobody".into();
    let fields: Vec<&str> = draft.errors().into_iter().map(|(field, _)| field).collect();
    assert_eq!(fields, ["host", "port", "from_address"]);
    draft.port = "0".into();
    assert!(draft.errors().iter().any(|(field, _)| *field == "port"));
    assert!(draft.to_request(true).is_some());
    draft.port = "x".into();
    assert!(draft.to_request(true).is_none());
  }

  #[test]
  fn hosts_may_be_names_or_addresses() {
    assert!(is_plausible_host("smtp.example.com"));
    assert!(is_plausible_host("192.0.2.10"));
    assert!(is_plausible_host("2001:db8::1"));
    assert!(!is_plausible_host("not a host"));
  }

  #[test]
  fn every_label_is_translated() {
    use crate::i18n::{Locale, translate};

    let mut keys = vec![
      "admin-mail-title",
      "admin-mail-subtitle",
      "admin-mail-server",
      "admin-mail-none",
      "admin-mail-host",
      "admin-mail-port",
      "admin-mail-security",
      "admin-mail-username",
      "admin-mail-password",
      "admin-mail-password-hint",
      "admin-mail-clear-password",
      "admin-mail-from-address",
      "admin-mail-from-address-hint",
      "admin-mail-from-name",
      "admin-mail-status",
      "admin-mail-never",
      "admin-mail-last-success",
      "admin-mail-last-failure",
      "admin-mail-last-error",
      "admin-mail-test-hint",
      "admin-mail-test-to",
      "admin-mail-test-send",
      "admin-mail-senders",
      "admin-mail-senders-hint",
      "admin-mail-sender-default",
      "admin-mail-open-brands",
      "admin-brands-name",
      "admin-settings-loading",
      "admin-secrets-unavailable",
      "admin-save",
      "admin-reset-to-config",
      "admin-test-running",
      "admin-test-ok",
      "admin-test-failed",
      "admin-error-host",
      "admin-error-port",
      "admin-error-email",
      "admin-error-too-long",
    ];
    keys.extend(SECURITY_MODES.iter().map(|(_, key)| *key));
    for locale in [Locale::En, Locale::De] {
      for key in &keys {
        assert_ne!(&translate(locale, key, None), key, "{locale:?} lacks {key}");
      }
    }
  }
}
