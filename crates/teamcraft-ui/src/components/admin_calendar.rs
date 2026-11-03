use leptos::prelude::*;
use teamcraft_proto::{CalDavSettings, SettingSource, UpdateCalendarSettingsRequest};

use crate::admin::SettingsTestKind;
use crate::components::admin_console::SectionHeader;
use crate::components::admin_fields::{
  CheckField, FieldErrors, SourceBadge, TextField, Validated, accept_or_refuse, bind, bind_bool, is_web_url,
};
use crate::components::admin_mail::secret_update;
use crate::i18n::use_i18n;
use crate::state::use_app_state;

const FORM_ID: &str = "admin-calendar-form";
const MAX_TEXT_CHARS: usize = 500;
/// The placeholder of the free/busy URL template that stands for a person's address.
const EMAIL_PLACEHOLDER: &str = "{email}";

/// The CalDAV calendar meetings are written to. Without `[meetings]` in the configuration there is
/// nothing to store, and the section says so.
#[component]
pub fn CalendarSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let current = Memo::new(move |_| state.read(|backend| backend.calendar_settings()));
  let caldav = Memo::new(move |_| current.get().and_then(|calendar| calendar.maybe_caldav));
  let draft = RwSignal::new(CalendarDraft::from_settings(caldav.get_untracked().as_ref()));
  let show_errors = RwSignal::new(false);
  let is_testing = RwSignal::new(false);
  Effect::new(move |previous: Option<()>| {
    let latest = caldav.get();
    if previous.is_some() {
      draft.set(CalendarDraft::from_settings(latest.as_ref()));
      show_errors.set(false);
    }
  });
  let is_available = Memo::new(move |_| current.get().is_some_and(|calendar| calendar.is_available));
  let can_store_secrets =
    Memo::new(move |_| current.get().is_some_and(|calendar| calendar.can_store_secrets));
  let source = Memo::new(move |_| {
    current
      .get()
      .map_or(SettingSource::Config, |calendar| calendar.source)
  });
  let has_password = Memo::new(move |_| caldav.get().is_some_and(|caldav| caldav.has_password));
  let test_result = Memo::new(move |_| {
    state
      .read(|backend| backend.last_settings_test())
      .filter(|(kind, _)| *kind == SettingsTestKind::Calendar)
      .map(|(_, response)| response)
  });
  Effect::new(move |_| {
    if test_result.get().is_some() {
      is_testing.set(false);
    }
  });

  let url = bind(
    draft,
    show_errors,
    "calendar_url",
    |d| &d.calendar_url,
    |d| &mut d.calendar_url,
  );
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
  let template = bind(
    draft,
    show_errors,
    "free_busy_url_template",
    |d| &d.free_busy_url_template,
    |d| &mut d.free_busy_url_template,
  );
  let (is_clearing, set_clearing) = bind_bool(
    draft,
    |d| d.is_clearing_password,
    |d, v| d.is_clearing_password = v,
  );

  let save = move |ev: web_sys::SubmitEvent| {
    ev.prevent_default();
    if !accept_or_refuse(draft, show_errors, FORM_ID) {
      return;
    }
    let request = draft.with_untracked(|draft| draft.to_request(can_store_secrets.get_untracked()));
    state.write(|backend| backend.save_calendar_settings(request));
  };

  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-calendar-title"))
      subtitle=Signal::derive(move || i18n.t("admin-calendar-subtitle"))
    />
    {move || {
      if current.get().is_none() {
        view! { <p class="muted-text" role="status">{i18n.t("admin-settings-loading")}</p> }.into_any()
      } else if !is_available.get() {
        view! { <p class="auth-alert policy-blocked" role="status">{i18n.t("admin-calendar-unavailable")}</p> }.into_any()
      } else {
        ().into_any()
      }
    }}
    <form id=FORM_ID class="policy-form" on:submit=save hidden=move || !is_available.get()>
      <fieldset class="policy-group">
        <legend>{move || i18n.t("admin-calendar-server")}</legend>
        <p class="settings-source small">
          <SourceBadge source=source />
          {move || {
            caldav.get().is_none().then(|| view! { <span class="muted-text">{i18n.t("admin-calendar-none")}</span> })
          }}
        </p>
        <TextField
          id="calendar-url"
          label="admin-calendar-url"
          placeholder="https://calendar.example.com/dav/meetings/"
          value=url.value
          on_input=url.on_input
          error=url.error
        />
        <TextField
          id="calendar-username"
          label="admin-calendar-username"
          value=username.value
          on_input=username.on_input
          error=username.error
        />
        <TextField
          id="calendar-password"
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
          id="calendar-free-busy"
          label="admin-calendar-free-busy"
          placeholder="https://calendar.example.com/dav/{email}/"
          hint="admin-calendar-free-busy-hint"
          value=template.value
          on_input=template.on_input
          error=template.error
        />
        <div class="policy-footer">
          <button type="submit" class="btn btn-primary">{move || i18n.t("admin-save")}</button>
          <button
            type="button"
            class="btn"
            prop:disabled=move || source.get() != SettingSource::Database
            on:click=move |_| state.write(|backend| backend.reset_calendar_settings())
          >
            {move || i18n.t("admin-reset-to-config")}
          </button>
        </div>
      </fieldset>
      <section class="policy-group">
        <h3>{move || i18n.t("admin-calendar-test")}</h3>
        <p class="muted-text small">{move || i18n.t("admin-calendar-test-hint")}</p>
        <button
          type="button"
          class="btn"
          prop:disabled=move || caldav.get().is_none() || is_testing.get()
          on:click=move |_| {
            is_testing.set(true);
            state.write(|backend| backend.test_calendar());
          }
        >
          {move || i18n.t("admin-calendar-test-run")}
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
    </form>
  }
}

/// What the calendar form holds, as typed.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CalendarDraft {
  calendar_url: String,
  username: String,
  password: String,
  is_clearing_password: bool,
  free_busy_url_template: String,
}

impl CalendarDraft {
  fn from_settings(maybe_caldav: Option<&CalDavSettings>) -> Self {
    Self {
      calendar_url: maybe_caldav
        .map(|caldav| caldav.calendar_url.clone())
        .unwrap_or_default(),
      username: maybe_caldav
        .map(|caldav| caldav.username.clone())
        .unwrap_or_default(),
      password: String::new(),
      is_clearing_password: false,
      free_busy_url_template: maybe_caldav
        .and_then(|caldav| caldav.maybe_free_busy_url_template.clone())
        .unwrap_or_default(),
    }
  }

  fn to_request(&self, can_store_secrets: bool) -> UpdateCalendarSettingsRequest {
    let template = self.free_busy_url_template.trim();
    UpdateCalendarSettingsRequest {
      calendar_url: self.calendar_url.trim().to_owned(),
      username: self.username.trim().to_owned(),
      password: secret_update(&self.password, self.is_clearing_password, can_store_secrets),
      maybe_free_busy_url_template: (!template.is_empty()).then(|| template.to_owned()),
    }
  }
}

impl Validated for CalendarDraft {
  fn errors(&self) -> FieldErrors {
    let mut errors = FieldErrors::new();
    if !is_web_url(self.calendar_url.trim()) {
      errors.push(("calendar_url", "admin-error-url"));
    }
    let username = self.username.trim();
    if username.is_empty() || username.chars().count() > MAX_TEXT_CHARS {
      errors.push(("username", "admin-error-required"));
    }
    let template = self.free_busy_url_template.trim();
    if !template.is_empty() && !(is_web_url(template) && template.contains(EMAIL_PLACEHOLDER)) {
      errors.push(("free_busy_url_template", "admin-error-template"));
    }
    errors
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn settings() -> CalDavSettings {
    CalDavSettings {
      calendar_url: "https://calendar.example.com/dav/meetings/".into(),
      username: "teamcraft".into(),
      has_password: true,
      maybe_free_busy_url_template: Some("https://calendar.example.com/dav/{email}/".into()),
    }
  }

  #[test]
  fn a_valid_draft_becomes_a_request_that_keeps_the_password() {
    let draft = CalendarDraft::from_settings(Some(&settings()));
    assert!(draft.errors().is_empty());
    let request = draft.to_request(true);
    assert_eq!(request.password, teamcraft_proto::SecretUpdate::Keep);
    assert_eq!(
      request.maybe_free_busy_url_template.as_deref(),
      Some("https://calendar.example.com/dav/{email}/")
    );
  }

  #[test]
  fn urls_and_the_template_are_checked() {
    let mut draft = CalendarDraft::from_settings(None);
    let fields = |draft: &CalendarDraft| -> Vec<&'static str> {
      draft.errors().into_iter().map(|(field, _)| field).collect()
    };
    assert_eq!(fields(&draft), ["calendar_url", "username"]);
    draft.calendar_url = "http://calendar.example.com".into();
    draft.username = "teamcraft".into();
    draft.free_busy_url_template = "https://calendar.example.com/dav/".into();
    assert_eq!(fields(&draft), ["calendar_url", "free_busy_url_template"]);
    draft.calendar_url = "https://calendar.example.com".into();
    draft.free_busy_url_template.clear();
    assert!(fields(&draft).is_empty());
    assert_eq!(draft.to_request(true).maybe_free_busy_url_template, None);
  }

  #[test]
  fn every_label_is_translated() {
    use crate::i18n::{Locale, translate};

    let keys = [
      "admin-calendar-title",
      "admin-calendar-subtitle",
      "admin-calendar-unavailable",
      "admin-calendar-server",
      "admin-calendar-none",
      "admin-calendar-url",
      "admin-calendar-username",
      "admin-calendar-free-busy",
      "admin-calendar-free-busy-hint",
      "admin-calendar-test",
      "admin-calendar-test-hint",
      "admin-calendar-test-run",
      "admin-mail-password",
      "admin-mail-password-hint",
      "admin-mail-clear-password",
      "admin-settings-loading",
      "admin-secrets-unavailable",
      "admin-save",
      "admin-reset-to-config",
      "admin-test-running",
      "admin-test-ok",
      "admin-test-failed",
      "admin-error-url",
      "admin-error-required",
      "admin-error-template",
    ];
    for locale in [Locale::En, Locale::De] {
      for key in keys {
        assert_ne!(translate(locale, key, None), key, "{locale:?} lacks {key}");
      }
    }
  }
}
