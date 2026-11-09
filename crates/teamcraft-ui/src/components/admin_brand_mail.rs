//! The "Own mail server" part of the brand editor (admin.md §6.2): a mail server only this brand
//! uses, with its own test mail and delivery status. Its own form: it is saved apart from the brand.

use leptos::prelude::*;
use teamcraft_proto::{
  AdminBrandsResponse, BrandMailServer, MailDeliveryStatus, SmtpSecurityMode, UpdateBrandMailServerRequest,
};

use crate::admin::SettingsTestKind;
use crate::components::admin_fields::{
  CheckField, FieldErrors, TextField, Validated, accept_or_refuse, bind, bind_bool, parse_number,
};
use crate::components::admin_mail::{
  DEFAULT_PORT, MAX_TEXT_CHARS, MailDelivery, SECURITY_MODES, is_plausible_host, secret_update,
};
use crate::components::admin_policy::SelectRow;
use crate::i18n::use_i18n;
use crate::state::use_app_state;

const FORM_ID: &str = "admin-brand-mail-form";

/// The form of a brand's own mail server. An empty host means the instance's mail server.
#[component]
pub fn BrandMailServerForm(brand_id: String, response: Memo<Option<AdminBrandsResponse>>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let lookup_id = brand_id.clone();
  let brand = Memo::new(move |_| {
    response
      .get()
      .and_then(|all| all.brands.into_iter().find(|brand| brand.id == lookup_id))
  });
  // Without the status: a test mail changes that, and must not reset what is being typed.
  let saved = Memo::new(move |_| {
    brand
      .get()
      .and_then(|brand| brand.maybe_mail_server)
      .map(|server| MailServerDraft::from_server(Some(&server)))
  });
  let draft = RwSignal::new(
    saved
      .get_untracked()
      .unwrap_or_else(|| MailServerDraft::from_server(None)),
  );
  let show_errors = RwSignal::new(false);
  Effect::new(move |previous: Option<()>| {
    let latest = saved.get();
    if previous.is_some() {
      draft.set(latest.unwrap_or_else(|| MailServerDraft::from_server(None)));
      show_errors.set(false);
    }
  });
  let can_store_secrets = Memo::new(move |_| response.get().is_some_and(|all| all.can_store_secrets));
  let has_server = Memo::new(move |_| saved.get().is_some());
  let has_password = Memo::new(move |_| saved.get().is_some_and(|server| server.has_password));
  let has_sender = Memo::new(move |_| {
    brand
      .get()
      .is_some_and(|brand| brand.settings.mail.maybe_from_address.is_some())
  });
  let status = Signal::derive(move || {
    brand
      .get()
      .and_then(|brand| brand.maybe_mail_server)
      .map_or_else(MailDeliveryStatus::default, |server| server.status)
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
  let (is_clearing, set_clearing) = bind_bool(
    draft,
    |d| d.is_clearing_password,
    |d, value| d.is_clearing_password = value,
  );
  let security_options = Signal::derive(move || {
    SECURITY_MODES
      .into_iter()
      .map(|(slug, key)| (slug.to_owned(), i18n.t(key)))
      .collect()
  });

  let save_id = brand_id.clone();
  let save = move |ev: web_sys::SubmitEvent| {
    ev.prevent_default();
    if !accept_or_refuse(draft, show_errors, FORM_ID) {
      return;
    }
    let request = draft.with_untracked(|draft| draft.to_request(can_store_secrets.get_untracked()));
    if let Some(request) = request {
      let id = save_id.clone();
      state.write(|backend| backend.save_brand_mail_server(id, request));
    }
  };
  let remove_id = brand_id.clone();
  let test_id = brand_id.clone();
  let on_send = Callback::new(move |request| {
    let id = test_id.clone();
    state.write(|backend| backend.send_brand_test_mail(id, request));
  });

  view! {
    <form id=FORM_ID class="brand-editor" on:submit=save>
      <fieldset class="policy-group">
        <legend>{move || i18n.t("admin-brands-own-mail")}</legend>
        <p class="muted-text small">{move || i18n.t("admin-brands-own-mail-hint")}</p>
        <p class="muted-text small" role="status">
          {move || {
            if has_server.get() { i18n.t("admin-brands-own-mail-active") } else { i18n.t("admin-brands-own-mail-instance") }
          }}
        </p>
        <TextField
          id="brand-mail-host"
          label="admin-mail-host"
          placeholder="smtp.example.com"
          value=host.value
          on_input=host.on_input
          error=host.error
        />
        <TextField
          id="brand-mail-port"
          label="admin-mail-port"
          input_type="number"
          value=port.value
          on_input=port.on_input
          error=port.error
        />
        <SelectRow
          id="brand-mail-security"
          label=Signal::derive(move || i18n.t("admin-mail-security"))
          options=security_options
          selected=Signal::derive(move || draft.with(|d| d.security.clone()))
          on_select=Callback::new(move |slug: String| draft.update(|d| d.security = slug))
        />
        <TextField
          id="brand-mail-username"
          label="admin-mail-username"
          value=username.value
          on_input=username.on_input
          error=username.error
        />
        <TextField
          id="brand-mail-password"
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
        {move || {
          (!has_sender.get())
            .then(|| view! { <p class="auth-alert policy-blocked" role="status">{i18n.t("admin-brands-own-mail-needs-sender")}</p> })
        }}
        <div class="policy-footer">
          <button type="submit" class="btn btn-primary" prop:disabled=move || !has_sender.get() && !has_server.get()>
            {move || i18n.t("admin-save")}
          </button>
          <button
            type="button"
            class="btn"
            prop:disabled=move || !has_server.get()
            on:click=move |_| {
              let id = remove_id.clone();
              state.write(|backend| backend.remove_brand_mail_server(id));
            }
          >
            {move || i18n.t("admin-brands-own-mail-remove")}
          </button>
        </div>
      </fieldset>
    </form>
    <section class="policy-group policy-form" aria-label=move || i18n.t("admin-mail-status")>
      <h3>{move || i18n.t("admin-mail-status")}</h3>
      <MailDelivery
        field_id="brand-mail-test-to"
        kind=SettingsTestKind::BrandMail(brand_id)
        status=status
        can_test=Signal::derive(move || has_server.get())
        on_send=on_send
      />
    </section>
  }
}

/// What the form holds: texts as typed, so a half-typed port is not lost.
#[derive(Clone, Debug, PartialEq, Eq)]
struct MailServerDraft {
  host: String,
  port: String,
  security: String,
  username: String,
  password: String,
  is_clearing_password: bool,
  has_password: bool,
  /// An empty host only means something when there is a server to remove.
  has_saved_server: bool,
}

impl MailServerDraft {
  fn from_server(maybe_server: Option<&BrandMailServer>) -> Self {
    match maybe_server {
      Some(server) => Self {
        host: server.host.clone(),
        port: server.port.to_string(),
        security: server.security.as_str().to_owned(),
        username: server.maybe_username.clone().unwrap_or_default(),
        password: String::new(),
        is_clearing_password: false,
        has_password: server.has_password,
        has_saved_server: true,
      },
      None => Self {
        host: String::new(),
        port: DEFAULT_PORT.to_string(),
        security: SmtpSecurityMode::StartTls.as_str().to_owned(),
        username: String::new(),
        password: String::new(),
        is_clearing_password: false,
        has_password: false,
        has_saved_server: false,
      },
    }
  }

  /// `None` if the port is not a number; check [`Validated::errors`] first.
  fn to_request(&self, can_store_secrets: bool) -> Option<UpdateBrandMailServerRequest> {
    let username = self.username.trim();
    let host = self.host.trim();
    Some(UpdateBrandMailServerRequest {
      host: host.to_owned(),
      // An empty host removes the server; what the port field holds then doesn't matter.
      port: parse_number::<u16>(&self.port).or(host.is_empty().then_some(DEFAULT_PORT))?,
      security: SmtpSecurityMode::from(self.security.clone()),
      maybe_username: (!username.is_empty()).then(|| username.to_owned()),
      password: secret_update(&self.password, self.is_clearing_password, can_store_secrets),
    })
  }
}

impl Validated for MailServerDraft {
  fn errors(&self) -> FieldErrors {
    let mut errors = FieldErrors::new();
    let host = self.host.trim();
    if host.is_empty() {
      if !self.has_saved_server {
        errors.push(("host", "admin-error-host"));
      }
      return errors;
    }
    if !is_plausible_host(host) {
      errors.push(("host", "admin-error-host"));
    }
    if parse_number::<u16>(&self.port).is_none_or(|port| port == 0) {
      errors.push(("port", "admin-error-port"));
    }
    if self.username.chars().count() > MAX_TEXT_CHARS {
      errors.push(("username", "admin-error-too-long"));
    }
    errors
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{SecretUpdate, WriteOnlySecret};

  use super::*;

  fn server() -> BrandMailServer {
    BrandMailServer {
      host: "smtp.brand-a.example.com".into(),
      port: 465,
      security: SmtpSecurityMode::Tls,
      maybe_username: Some("mailer".into()),
      has_password: true,
      status: MailDeliveryStatus::default(),
    }
  }

  #[test]
  fn the_draft_round_trips_and_keeps_the_password() {
    let draft = MailServerDraft::from_server(Some(&server()));
    assert!(draft.errors().is_empty());
    let request = draft.to_request(true).unwrap();
    assert_eq!(request.host, "smtp.brand-a.example.com");
    assert_eq!(request.port, 465);
    assert_eq!(request.security, SmtpSecurityMode::Tls);
    assert_eq!(request.password, SecretUpdate::Keep);
  }

  #[test]
  fn a_typed_password_replaces_the_stored_one() {
    let mut draft = MailServerDraft::from_server(Some(&server()));
    draft.password = "new-secret".into();
    assert_eq!(
      draft.to_request(true).unwrap().password,
      SecretUpdate::Set(WriteOnlySecret::new("new-secret".into()))
    );
  }

  #[test]
  fn an_empty_host_removes_a_server_and_is_a_mistake_without_one() {
    let mut saved = MailServerDraft::from_server(Some(&server()));
    saved.host = "  ".into();
    saved.port = "x".into();
    assert!(saved.errors().is_empty());
    assert_eq!(saved.to_request(true).unwrap().host, "");

    let fresh = MailServerDraft::from_server(None);
    let fields: Vec<&str> = fresh.errors().into_iter().map(|(field, _)| field).collect();
    assert_eq!(fields, ["host"]);
  }

  #[test]
  fn obvious_mistakes_are_reported_per_field() {
    let mut draft = MailServerDraft::from_server(None);
    draft.host = "https://smtp.example.com".into();
    draft.port = "70000".into();
    let fields: Vec<&str> = draft.errors().into_iter().map(|(field, _)| field).collect();
    assert_eq!(fields, ["host", "port"]);
  }

  #[test]
  fn every_label_is_translated() {
    use crate::i18n::{Locale, translate};

    for locale in [Locale::En, Locale::De] {
      for key in [
        "admin-brands-own-mail",
        "admin-brands-own-mail-hint",
        "admin-brands-own-mail-active",
        "admin-brands-own-mail-instance",
        "admin-brands-own-mail-needs-sender",
        "admin-brands-own-mail-remove",
      ] {
        assert_ne!(translate(locale, key, None), key, "{locale:?} lacks {key}");
      }
    }
  }
}
