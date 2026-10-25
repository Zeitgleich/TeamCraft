use std::collections::BTreeMap;

use leptos::prelude::*;
use serde_json::{Map, Value, json};
use teamcraft_proto::{
  RuntimeSettingsKey, RuntimeSettingsResponse, RuntimeSettingsSection, SettingSource, Snowflake,
};

use crate::components::admin_console::SectionHeader;
use crate::components::admin_fields::{
  CheckField, FieldErrors, SourceBadge, TextAreaField, TextField, format_scaled, is_web_url, list_text,
  parse_list, parse_scaled,
};
use crate::i18n::use_i18n;
use crate::state::use_app_state;

const MIB: u64 = 1_048_576;
const MINUTE: u64 = 60;
/// Form values by field id: booleans as `true` / `false`, numbers in the shown unit, lists as text.
type Values = BTreeMap<&'static str, String>;

/// Groups in the order of the page.
const GROUPS: [&str; 10] = [
  "server_rules",
  "notifications",
  "email_digest",
  "shares",
  "limits",
  "integration",
  "local_accounts",
  "sign_in_page",
  "mail_texts",
  "meeting_mail",
];
/// The sign-in methods an admin can choose, as the form's values: comma-separated, in order.
const METHOD_CHOICES: [&str; 4] = ["sso,local", "local,sso", "sso", "local"];

/// How a field is edited and checked.
#[derive(Clone, Copy)]
enum Kind {
  Bool,
  /// A whole number of raw units, shown as `raw / scale` in the unit named by the i18n key.
  Number {
    scale: u64,
    max: u64,
    unit: Option<&'static str>,
  },
  Text,
  Area,
  /// Space or channel IDs.
  IdList,
  /// The sign-in methods in order, one of [`METHOD_CHOICES`].
  Methods,
  /// One language's entry of a text map (`help_text.de`); empty means none.
  MapText,
  /// An https link that may be empty (none).
  Link,
}

#[derive(Clone, Copy)]
struct FieldDef {
  id: &'static str,
  kind: Kind,
}

const fn flag(id: &'static str) -> FieldDef {
  FieldDef { id, kind: Kind::Bool }
}

const fn count(id: &'static str) -> FieldDef {
  FieldDef {
    id,
    kind: Kind::Number {
      scale: 1,
      max: u32::MAX as u64,
      unit: None,
    },
  }
}

const fn amount(id: &'static str, scale: u64, max: u64, unit: &'static str) -> FieldDef {
  FieldDef {
    id,
    kind: Kind::Number {
      scale,
      max,
      unit: Some(unit),
    },
  }
}

const fn text(id: &'static str) -> FieldDef {
  FieldDef { id, kind: Kind::Text }
}

const fn area(id: &'static str) -> FieldDef {
  FieldDef { id, kind: Kind::Area }
}

const fn map_text(id: &'static str) -> FieldDef {
  FieldDef {
    id,
    kind: Kind::MapText,
  }
}

const SECONDS: &str = "settings-unit-seconds";
const MINUTES: &str = "settings-unit-minutes";
const HOURS: &str = "settings-unit-hours";
const DAYS: &str = "settings-unit-days";
const MEGABYTES: &str = "settings-unit-mib";
const U32_MAX: u64 = u32::MAX as u64;
const U64_MAX: u64 = 9_000_000_000_000_000;

const SERVER_RULES: &[FieldDef] = &[
  flag("invites_require_verified_email"),
  FieldDef {
    id: "default_spaces",
    kind: Kind::IdList,
  },
  FieldDef {
    id: "default_channels",
    kind: Kind::IdList,
  },
];
const NOTIFICATIONS: &[FieldDef] = &[
  flag("read_receipts_default"),
  amount("urgent_repeat_interval_secs", 1, U64_MAX, SECONDS),
  count("urgent_max_pushes"),
  amount("mobile_push_delay_secs", 1, U64_MAX, SECONDS),
  flag("is_desktop_attention_enabled"),
];
const EMAIL_DIGEST: &[FieldDef] = &[
  flag("is_enabled"),
  amount("default_interval_secs", MINUTE, U64_MAX, MINUTES),
  amount("min_interval_secs", MINUTE, U64_MAX, MINUTES),
  amount("grace_secs", MINUTE, U64_MAX, MINUTES),
  text("subject_one"),
  text("subject_many"),
  area("body_one"),
  area("body_many"),
];
const SHARES: &[FieldDef] = &[
  flag("is_enabled"),
  amount("default_expiry_days", 1, U32_MAX, DAYS),
  amount("max_expiry_days", 1, U32_MAX, DAYS),
  count("max_downloads_cap"),
  count("max_links_per_user"),
];
const LIMITS: &[FieldDef] = &[
  amount("max_attachment_bytes", MIB, U64_MAX, MEGABYTES),
  amount("max_attachment_bytes_per_user", MIB, U64_MAX, MEGABYTES),
  amount("max_avatar_bytes", MIB, U64_MAX, MEGABYTES),
  amount("max_space_image_bytes", MIB, U64_MAX, MEGABYTES),
  amount("max_background_bytes", MIB, U64_MAX, MEGABYTES),
  count("max_backgrounds_per_user"),
  count("max_group_dm_participants"),
  count("max_user_group_members"),
  count("max_meeting_mails_per_user_per_hour"),
];
const INTEGRATION: &[FieldDef] = &[amount("default_retention_days", 1, U32_MAX, DAYS)];
const LOCAL_ACCOUNTS: &[FieldDef] = &[
  flag("is_enabled"),
  flag("require_totp"),
  amount("activation_ttl_hours", 1, U32_MAX, HOURS),
  amount("session_ttl_hours", 1, U32_MAX, HOURS),
  count("max_failed_attempts"),
  amount("lockout_secs", MINUTE, U64_MAX, MINUTES),
];
const SIGN_IN_PAGE: &[FieldDef] = &[
  FieldDef {
    id: "methods",
    kind: Kind::Methods,
  },
  map_text("help_text.en"),
  map_text("help_text.de"),
  FieldDef {
    id: "help_link",
    kind: Kind::Link,
  },
  flag("is_login_page_always_shown"),
];
const MAIL_TEXTS: &[FieldDef] = &[
  text("invite_subject"),
  area("invite_body"),
  text("activation_subject"),
  area("activation_body"),
];
const MEETING_MAIL: &[FieldDef] = &[
  count("max_invitees"),
  text("invitation_subject"),
  area("invitation_body"),
  text("update_subject"),
  area("update_body"),
  text("cancellation_subject"),
  area("cancellation_body"),
];

/// Instance settings that used to need the configuration file: one form per group, each with its
/// own save and a way back to the file's value.
#[component]
pub fn SettingsSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let loaded = Memo::new(move |_| state.read(|backend| backend.runtime_settings().is_some()));
  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-settings-title"))
      subtitle=Signal::derive(move || i18n.t("admin-settings-subtitle"))
    />
    {move || (!loaded.get()).then(|| view! { <p class="muted-text" role="status">{i18n.t("admin-settings-loading")}</p> })}
    <div class="policy-form">
      {GROUPS.into_iter().map(|slug| view! { <GroupForm slug=slug /> }).collect_view()}
    </div>
  }
}

#[component]
fn GroupForm(slug: &'static str) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let key = RuntimeSettingsKey::from(slug.to_owned());
  let stored = {
    let key = key.clone();
    Memo::new(move |_| {
      state
        .read(|backend| backend.runtime_settings())
        .map(|response| to_values(&key, &response))
    })
  };
  let draft = RwSignal::new(stored.get_untracked().unwrap_or_default());
  let show_errors = RwSignal::new(false);
  Effect::new(move |previous: Option<()>| {
    let latest = stored.get();
    // The first run only subscribes; later changes of the stored values replace the draft.
    if previous.is_some()
      && let Some(values) = latest
    {
      draft.set(values);
      show_errors.set(false);
    }
  });
  let source = {
    let key = key.clone();
    Memo::new(move |_| {
      state
        .read(|backend| backend.runtime_settings())
        .and_then(|response| {
          response
            .sources
            .into_iter()
            .find(|state| state.key == key)
            .map(|state| state.source)
        })
        .unwrap_or(SettingSource::Config)
    })
  };
  let group_error = Signal::derive({
    let key = key.clone();
    move || {
      show_errors
        .get()
        .then(|| draft.with(|values| check(&key, values).err()))
        .flatten()
        .filter(|errors| errors.iter().any(|(field, _)| *field == "form"))
        .map(|_| "admin-error-invalid")
    }
  });
  let save = {
    let key = key.clone();
    move |ev: web_sys::SubmitEvent| {
      ev.prevent_default();
      show_errors.set(true);
      if let Ok(section) = draft.with_untracked(|values| check(&key, values)) {
        state.write(|backend| backend.save_runtime_settings(section));
      }
    }
  };
  let reset_key = key.clone();
  let title = format!("settings-group-{slug}");
  let hint = format!("settings-group-{slug}-hint");
  view! {
    <form on:submit=save>
      <fieldset class="policy-group">
        <legend>{move || i18n.t(&title)}</legend>
        <p class="settings-source small">
          <SourceBadge source=source />
          <span class="muted-text">{move || i18n.t(&hint)}</span>
        </p>
        {fields_of(slug)
          .iter()
          .map(|field| view! { <FieldRow slug=slug field=*field draft=draft show_errors=show_errors /> })
          .collect_view()}
        {move || group_error.get().map(|key| view! { <p class="field-error" role="alert">{i18n.t(key)}</p> })}
        <div class="policy-footer">
          <button type="submit" class="btn btn-primary">{move || i18n.t("admin-save")}</button>
          <button
            type="button"
            class="btn"
            prop:disabled=move || source.get() != SettingSource::Database
            on:click=move |_| {
              let key = reset_key.clone();
              state.write(|backend| backend.reset_runtime_settings(key));
            }
          >
            {move || i18n.t("admin-reset-to-config")}
          </button>
        </div>
      </fieldset>
    </form>
  }
}

#[component]
fn FieldRow(
  slug: &'static str,
  field: FieldDef,
  draft: RwSignal<Values>,
  show_errors: RwSignal<bool>,
) -> impl IntoView {
  let id = field.id;
  let dom_id = format!("settings-{slug}-{}", id.replace('.', "-"));
  let label = leak_key(label_key(id));
  let value = Signal::derive(move || draft.with(|values| values.get(id).cloned().unwrap_or_default()));
  let on_input = Callback::new(move |text: String| {
    draft.update(|values| {
      values.insert(id, text);
    });
  });
  let key = RuntimeSettingsKey::from(slug.to_owned());
  let error = Signal::derive(move || {
    if !show_errors.get() {
      return None;
    }
    draft.with(|values| {
      check(&key, values).err().and_then(|errors| {
        errors
          .into_iter()
          .find(|(field, _)| *field == id)
          .map(|(_, key)| key)
      })
    })
  });
  match field.kind {
    Kind::Bool => view! {
      <CheckField
        label=label
        checked=Signal::derive(move || value.get() == "true")
        on_change=Callback::new(move |checked: bool| on_input.run(checked.to_string()))
      />
    }
    .into_any(),
    Kind::Number { unit, .. } => view! {
      <TextField id=leak_key(dom_id) label=label input_type="number" unit=unit.unwrap_or_default() value=value on_input=on_input error=error />
    }
    .into_any(),
    Kind::Text => view! {
      <TextField id=leak_key(dom_id) label=label value=value on_input=on_input error=error />
    }
    .into_any(),
    Kind::Area | Kind::MapText => view! {
      <TextAreaField id=leak_key(dom_id) label=label rows=3 value=value on_input=on_input error=error />
    }
    .into_any(),
    Kind::Link => view! {
      <TextField id=leak_key(dom_id) label=label input_type="url" placeholder="https://help.example.com" value=value on_input=on_input error=error />
    }
    .into_any(),
    Kind::Methods => {
      let i18n = use_i18n();
      let dom_id = leak_key(dom_id);
      view! {
        <div class="field">
          <label for=dom_id>{move || i18n.t(label)}</label>
          <select
            id=dom_id
            class="input"
            prop:value=move || value.get()
            on:change=move |ev| on_input.run(event_target_value(&ev))
          >
            {METHOD_CHOICES
              .into_iter()
              .map(|choice| {
                let key = method_choice_key(choice);
                view! { <option value=choice selected=move || value.get() == choice>{move || i18n.t(key)}</option> }
              })
              .collect_view()}
          </select>
        </div>
      }
      .into_any()
    },
    Kind::IdList => view! {
      <TextAreaField
        id=leak_key(dom_id)
        label=label
        hint="settings-list-hint"
        rows=2
        value=value
        on_input=on_input
        error=error
      />
    }
    .into_any(),
  }
}

/// The i18n key of a field's label (Fluent names have no dots).
fn label_key(id: &str) -> String {
  format!("settings-f-{}", id.replace('.', "-"))
}

/// The i18n key of a choice of sign-in methods.
fn method_choice_key(choice: &str) -> &'static str {
  match choice {
    "local,sso" => "settings-methods-local-sso",
    "sso" => "settings-methods-sso",
    "local" => "settings-methods-local",
    _ => "settings-methods-sso-local",
  }
}

/// The i18n key or DOM id of a field, which components take as `&'static str`. The set is the
/// fixed field tables above, so the few bytes leaked per field row are bounded.
fn leak_key(text: String) -> &'static str {
  Box::leak(text.into_boxed_str())
}

fn fields_of(slug: &str) -> &'static [FieldDef] {
  match slug {
    "server_rules" => SERVER_RULES,
    "notifications" => NOTIFICATIONS,
    "email_digest" => EMAIL_DIGEST,
    "shares" => SHARES,
    "limits" => LIMITS,
    "integration" => INTEGRATION,
    "local_accounts" => LOCAL_ACCOUNTS,
    "sign_in_page" => SIGN_IN_PAGE,
    "mail_texts" => MAIL_TEXTS,
    "meeting_mail" => MEETING_MAIL,
    _ => &[],
  }
}

/// The settings of one group as the JSON object the wire uses.
fn group_json(key: &RuntimeSettingsKey, response: &RuntimeSettingsResponse) -> Option<Value> {
  let value = match key {
    RuntimeSettingsKey::ServerRules => serde_json::to_value(&response.server_rules),
    RuntimeSettingsKey::Notifications => serde_json::to_value(&response.notifications),
    RuntimeSettingsKey::EmailDigest => serde_json::to_value(&response.email_digest),
    RuntimeSettingsKey::Shares => serde_json::to_value(&response.shares),
    RuntimeSettingsKey::Limits => serde_json::to_value(&response.limits),
    RuntimeSettingsKey::Integration => serde_json::to_value(&response.integration),
    RuntimeSettingsKey::LocalAccounts => serde_json::to_value(&response.local_accounts),
    RuntimeSettingsKey::SignInPage => serde_json::to_value(&response.sign_in_page),
    RuntimeSettingsKey::MailTexts => serde_json::to_value(&response.mail_texts),
    RuntimeSettingsKey::MeetingMail => serde_json::to_value(&response.meeting_mail),
    _ => return None,
  };
  value.ok()
}

/// Form values for a group, from the settings in force.
fn to_values(key: &RuntimeSettingsKey, response: &RuntimeSettingsResponse) -> Values {
  let json = group_json(key, response).unwrap_or(Value::Null);
  fields_of(key.as_str())
    .iter()
    .map(|field| (field.id, value_text(field.kind, lookup(&json, field.id))))
    .collect()
}

/// A field's value in the group's JSON; `map.key` reaches into an object.
fn lookup<'a>(json: &'a Value, id: &str) -> Option<&'a Value> {
  match id.split_once('.') {
    Some((map, key)) => json.get(map)?.get(key),
    None => json.get(id),
  }
}

fn value_text(kind: Kind, maybe_value: Option<&Value>) -> String {
  let Some(value) = maybe_value else {
    return String::new();
  };
  match kind {
    Kind::Bool => value.as_bool().unwrap_or(false).to_string(),
    Kind::Number { scale, .. } => value
      .as_u64()
      .map(|raw| format_scaled(raw, scale))
      .unwrap_or_default(),
    Kind::Text | Kind::Area | Kind::MapText | Kind::Link => value.as_str().unwrap_or_default().to_owned(),
    Kind::Methods => value
      .as_array()
      .map(|methods| {
        methods
          .iter()
          .filter_map(Value::as_str)
          .collect::<Vec<_>>()
          .join(",")
      })
      .unwrap_or_default(),
    Kind::IdList => {
      let items: Vec<String> = value
        .as_array()
        .map(|items| {
          items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect()
        })
        .unwrap_or_default();
      list_text(&items)
    },
  }
}

/// The section to send for the typed values, or what is wrong with them. A problem that belongs to
/// no single field is reported under `form`.
fn check(key: &RuntimeSettingsKey, values: &Values) -> Result<RuntimeSettingsSection, FieldErrors> {
  let mut errors = FieldErrors::new();
  let mut object = Map::new();
  for field in fields_of(key.as_str()) {
    let typed = values.get(field.id).map(String::as_str).unwrap_or_default();
    match field_json(field.kind, typed) {
      // Empty optional values are left out (none).
      Ok(Value::Null) => {},
      Ok(value) => match field.id.split_once('.') {
        Some((map, key)) => {
          if let Some(entries) = object
            .entry(map.to_owned())
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
          {
            entries.insert(key.to_owned(), value);
          }
        },
        None => {
          object.insert(field.id.to_owned(), value);
        },
      },
      Err(message) => errors.push((field.id, message)),
    }
  }
  if !errors.is_empty() {
    return Err(errors);
  }
  serde_json::from_value(json!({ "section": key.as_str(), "values": object }))
    .map_err(|_| vec![("form", "admin-error-invalid")])
}

fn field_json(kind: Kind, typed: &str) -> Result<Value, &'static str> {
  match kind {
    Kind::Bool => Ok(Value::Bool(typed == "true")),
    Kind::Number { scale, max, .. } => match parse_scaled(typed, scale) {
      Some(raw) if raw <= max => Ok(json!(raw)),
      _ => Err("admin-error-number"),
    },
    Kind::Text | Kind::Area => Ok(Value::String(typed.trim_end().to_owned())),
    Kind::MapText => Ok(match typed.trim() {
      "" => Value::Null,
      text => Value::String(text.to_owned()),
    }),
    Kind::Link => match typed.trim() {
      "" => Ok(Value::Null),
      link if link.starts_with("https://") && is_web_url(link) => Ok(Value::String(link.to_owned())),
      _ => Err("admin-error-url"),
    },
    Kind::Methods => {
      let choice = METHOD_CHOICES
        .into_iter()
        .find(|choice| *choice == typed)
        .unwrap_or(METHOD_CHOICES[0]);
      Ok(json!(choice.split(',').collect::<Vec<_>>()))
    },
    Kind::IdList => {
      let items = parse_list(typed);
      if items.iter().all(|item| item.parse::<Snowflake>().is_ok()) {
        Ok(json!(items))
      } else {
        Err("admin-error-ids")
      }
    },
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn kind(slug: &str) -> RuntimeSettingsKey {
    RuntimeSettingsKey::from(slug.to_owned())
  }

  fn typed(pairs: &[(&'static str, &str)]) -> Values {
    pairs.iter().map(|(id, text)| (*id, (*text).to_owned())).collect()
  }

  #[test]
  fn every_group_has_fields_and_every_key_is_known() {
    for slug in GROUPS {
      assert!(!fields_of(slug).is_empty(), "{slug}");
      assert!(
        !matches!(kind(slug), RuntimeSettingsKey::Unknown(_)),
        "{slug} is not a settings key"
      );
    }
  }

  #[test]
  fn a_section_is_built_from_typed_values_in_the_shown_units() {
    let values = typed(&[
      ("is_enabled", "true"),
      ("default_interval_secs", "30"),
      ("min_interval_secs", "1.5"),
      ("grace_secs", "10"),
      ("subject_one", "One mention"),
      ("subject_many", "{count} mentions"),
      ("body_one", "Open {url}"),
      ("body_many", "Open {url}"),
    ]);
    let Ok(RuntimeSettingsSection::EmailDigest(digest)) = check(&kind("email_digest"), &values) else {
      panic!("the digest should be valid");
    };
    assert!(digest.is_enabled);
    assert_eq!(digest.default_interval_secs, 1_800);
    assert_eq!(digest.min_interval_secs, 90);
    assert_eq!(digest.grace_secs, 600);
  }

  #[test]
  fn lists_and_numbers_report_their_field() {
    let values = typed(&[
      ("is_enabled", "true"),
      ("default_expiry_days", "-3"),
      ("max_expiry_days", "30"),
      ("max_downloads_cap", "0"),
      ("max_links_per_user", "5"),
    ]);
    assert_eq!(
      check(&kind("shares"), &values).err(),
      Some(vec![("default_expiry_days", "admin-error-number")])
    );
    let ids = typed(&[
      ("invites_require_verified_email", "true"),
      ("default_spaces", "12, 13"),
      ("default_channels", "abc"),
    ]);
    assert_eq!(
      check(&kind("server_rules"), &ids).err(),
      Some(vec![("default_channels", "admin-error-ids")])
    );
  }

  #[test]
  fn ids_pass_as_strings() {
    let rules = typed(&[
      ("invites_require_verified_email", "true"),
      ("default_spaces", "12\n13"),
      ("default_channels", ""),
    ]);
    let Ok(RuntimeSettingsSection::ServerRules(rules)) = check(&kind("server_rules"), &rules) else {
      panic!("valid");
    };
    assert_eq!(rules.default_spaces.len(), 2);
    assert!(rules.default_channels.is_empty());
  }

  #[test]
  fn the_sign_in_page_maps_methods_help_texts_and_the_link() {
    let values = typed(&[
      ("methods", "local,sso"),
      ("help_text.en", "Call 1234."),
      ("help_text.de", "  "),
      ("help_link", ""),
    ]);
    let Ok(RuntimeSettingsSection::SignInPage(page)) = check(&kind("sign_in_page"), &values) else {
      panic!("valid");
    };
    assert_eq!(
      page.methods,
      [
        teamcraft_proto::SignInMethod::Local,
        teamcraft_proto::SignInMethod::Sso
      ]
    );
    assert_eq!(page.help_text.len(), 1);
    assert_eq!(page.help_text["en"], "Call 1234.");
    assert!(page.maybe_help_link.is_none());

    let response_json =
      json!({"methods": ["sso"], "help_text": {"de": "Hilfe"}, "help_link": "https://help.example.com"});
    assert_eq!(
      value_text(Kind::Methods, lookup(&response_json, "methods")),
      "sso"
    );
    assert_eq!(
      value_text(Kind::MapText, lookup(&response_json, "help_text.de")),
      "Hilfe"
    );
    assert_eq!(
      value_text(Kind::MapText, lookup(&response_json, "help_text.en")),
      ""
    );

    let bad_link = typed(&[("methods", "sso"), ("help_link", "http://help.example.com")]);
    assert_eq!(
      check(&kind("sign_in_page"), &bad_link).err(),
      Some(vec![("help_link", "admin-error-url")])
    );
  }

  #[test]
  fn too_large_counts_are_refused_before_sending() {
    let values = typed(&[("default_retention_days", "99999999999")]);
    assert_eq!(
      check(&kind("integration"), &values).err(),
      Some(vec![("default_retention_days", "admin-error-number")])
    );
  }

  #[test]
  fn every_label_is_translated() {
    use crate::i18n::{Locale, translate};

    let mut keys: Vec<String> = vec![
      "admin-settings-title".into(),
      "admin-settings-subtitle".into(),
      "admin-settings-loading".into(),
      "settings-list-hint".into(),
      "admin-save".into(),
      "admin-reset-to-config".into(),
      "admin-error-number".into(),
      "admin-error-ids".into(),
      "admin-error-invalid".into(),
    ];
    for unit in [SECONDS, MINUTES, HOURS, DAYS, MEGABYTES] {
      keys.push(unit.into());
    }
    for slug in GROUPS {
      keys.push(format!("settings-group-{slug}"));
      keys.push(format!("settings-group-{slug}-hint"));
      for field in fields_of(slug) {
        keys.push(label_key(field.id));
      }
    }
    for choice in METHOD_CHOICES {
      keys.push(method_choice_key(choice).into());
    }
    keys.push("admin-error-url".into());
    for locale in [Locale::En, Locale::De] {
      for key in &keys {
        assert_ne!(&translate(locale, key, None), key, "{locale:?} lacks {key}");
      }
    }
  }
}
