//! Form rows and input checks shared by the runtime-settings sections of the console (brands, mail,
//! calendar, instance settings). The checks only catch obvious mistakes early; the server validates
//! every request again.

use leptos::prelude::*;
use teamcraft_proto::SettingSource;

use crate::form_errors::{error_id, focus_first_invalid};
use crate::i18n::use_i18n;

/// A field id and the i18n key of what is wrong with it.
pub type FieldErrors = Vec<(&'static str, &'static str)>;

/// A form draft that can say what is wrong with it.
pub trait Validated {
  fn errors(&self) -> FieldErrors;
}

/// The reactive ends of one text field of a draft: its value, its edits and its error. Errors show
/// only once the form was submitted (`show_errors`), so a fresh form is not red.
#[derive(Clone, Copy)]
pub struct Bound {
  pub value: Signal<String>,
  pub on_input: Callback<String>,
  pub error: Signal<Option<&'static str>>,
}

pub fn bind<T: Validated + Send + Sync + 'static>(
  draft: RwSignal<T>,
  show_errors: RwSignal<bool>,
  field: &'static str,
  get: fn(&T) -> &String,
  set: fn(&mut T) -> &mut String,
) -> Bound {
  Bound {
    value: Signal::derive(move || draft.with(|draft| get(draft).clone())),
    on_input: Callback::new(move |text: String| draft.update(|draft| *set(draft) = text)),
    error: Signal::derive(move || {
      if show_errors.get() {
        draft.with(|draft| {
          draft
            .errors()
            .into_iter()
            .find(|(id, _)| *id == field)
            .map(|(_, key)| key)
        })
      } else {
        None
      }
    }),
  }
}

/// Checks a draft on submit: shows its errors and, when there are any, moves the focus to the first
/// invalid field inside the form `form_id` (forms.md). Returns whether the draft may be sent.
pub fn accept_or_refuse<T: Validated + Send + Sync + 'static>(
  draft: RwSignal<T>,
  show_errors: RwSignal<bool>,
  form_id: impl Into<String>,
) -> bool {
  show_errors.set(true);
  let is_valid = draft.with_untracked(|draft| draft.errors().is_empty());
  if !is_valid {
    focus_first_invalid(form_id);
  }
  is_valid
}

/// The reactive ends of a checkbox of a draft.
pub fn bind_bool<T: Send + Sync + 'static>(
  draft: RwSignal<T>,
  get: fn(&T) -> bool,
  set: fn(&mut T, bool),
) -> (Signal<bool>, Callback<bool>) {
  (
    Signal::derive(move || draft.with(get)),
    Callback::new(move |value: bool| draft.update(|draft| set(draft, value))),
  )
}

const MAX_HOSTNAME_CHARS: usize = 253;
const MAX_LABEL_CHARS: usize = 63;

/// Where a setting in force comes from: the configuration file or the database.
#[component]
pub fn SourceBadge(#[prop(into)] source: Signal<SettingSource>) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <span
      class="badge source-badge"
      class:badge-active=move || source.get() == SettingSource::Database
    >
      {move || i18n.t(source_key(&source.get()))}
    </span>
  }
}

/// A labelled single-line input with a hint, a unit and an inline error.
#[component]
pub fn TextField(
  id: &'static str,
  label: &'static str,
  #[prop(into)] value: Signal<String>,
  on_input: Callback<String>,
  #[prop(optional)] hint: Option<&'static str>,
  #[prop(optional)] placeholder: &'static str,
  #[prop(default = "text")] input_type: &'static str,
  #[prop(optional)] unit: &'static str,
  #[prop(into, optional)] error: Signal<Option<&'static str>>,
  #[prop(into, optional)] disabled: Signal<bool>,
) -> impl IntoView {
  let i18n = use_i18n();
  let hint_id = format!("{id}-hint");
  let describedby = described_by(id, hint.map(|_| hint_id.clone()), error);
  view! {
    <div class="field">
      <label for=id>{move || i18n.t(label)}</label>
      <div class="field-row">
        <input
          id=id
          class="input"
          type=input_type
          translate="no"
          placeholder=placeholder
          step=(input_type == "number").then_some("any")
          // Service credentials (SMTP, CalDAV), never the admin's own login: no autofill.
          autocomplete=(input_type == "password").then_some("new-password")
          aria-describedby=describedby
          aria-invalid=move || error.get().map(|_| "true")
          prop:disabled=move || disabled.get()
          prop:value=move || value.get()
          on:input=move |ev| on_input.run(event_target_value(&ev))
        />
        {(!unit.is_empty()).then(|| view! { <span class="field-unit muted-text">{move || i18n.t(unit)}</span> })}
      </div>
      {hint.map(|hint| view! { <p id=hint_id class="muted-text small">{move || i18n.t(hint)}</p> })}
      <p class="field-error" id=error_id(id)>{move || error.get().map(|key| i18n.t(key))}</p>
    </div>
  }
}

/// A labelled multi-line input (texts, lists).
#[component]
pub fn TextAreaField(
  id: &'static str,
  label: &'static str,
  #[prop(into)] value: Signal<String>,
  on_input: Callback<String>,
  #[prop(optional)] hint: Option<&'static str>,
  #[prop(default = 3)] rows: u32,
  #[prop(into, optional)] error: Signal<Option<&'static str>>,
  #[prop(into, optional)] disabled: Signal<bool>,
) -> impl IntoView {
  let i18n = use_i18n();
  let hint_id = format!("{id}-hint");
  let describedby = described_by(id, hint.map(|_| hint_id.clone()), error);
  view! {
    <div class="field">
      <label for=id>{move || i18n.t(label)}</label>
      <textarea
        id=id
        class="input"
        translate="no"
        rows=rows.to_string()
        aria-describedby=describedby
        aria-invalid=move || error.get().map(|_| "true")
        prop:disabled=move || disabled.get()
        prop:value=move || value.get()
        on:input=move |ev| on_input.run(event_target_value(&ev))
      ></textarea>
      {hint.map(|hint| view! { <p id=hint_id class="muted-text small">{move || i18n.t(hint)}</p> })}
      <p class="field-error" id=error_id(id)>{move || error.get().map(|key| i18n.t(key))}</p>
    </div>
  }
}

/// A checkbox with a label and an optional hint below it.
#[component]
pub fn CheckField(
  label: &'static str,
  #[prop(into)] checked: Signal<bool>,
  on_change: Callback<bool>,
  #[prop(optional)] hint: Option<&'static str>,
  #[prop(into, optional)] disabled: Signal<bool>,
) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <label class="checkbox check-field">
      <input
        type="checkbox"
        prop:checked=move || checked.get()
        prop:disabled=move || disabled.get()
        on:change=move |ev| on_change.run(event_target_checked(&ev))
      />
      <span>
        {move || i18n.t(label)}
        {hint.map(|hint| view! { <small class="muted-text">{move || i18n.t(hint)}</small> })}
      </span>
    </label>
  }
}

/// `aria-describedby` of a field row: its hint, and its message while it has an error.
fn described_by(
  id: &'static str,
  maybe_hint_id: Option<String>,
  error: Signal<Option<&'static str>>,
) -> impl Fn() -> Option<String> + Send + Sync + 'static {
  move || {
    let maybe_error_id = error.get().map(|_| error_id(id));
    let ids: Vec<String> = maybe_hint_id.iter().cloned().chain(maybe_error_id).collect();
    (!ids.is_empty()).then(|| ids.join(" "))
  }
}

pub fn source_key(source: &SettingSource) -> &'static str {
  match source {
    SettingSource::Database => "admin-source-database",
    _ => "admin-source-config",
  }
}

/// Entries of a list typed as text: separated by commas, semicolons or line breaks; blanks and
/// repeats are dropped and the order is kept.
pub fn parse_list(text: &str) -> Vec<String> {
  let mut items: Vec<String> = Vec::new();
  for part in text.split([',', ';', '\n', '\r']) {
    let item = part.trim();
    if !item.is_empty() && !items.iter().any(|existing| existing == item) {
      items.push(item.to_owned());
    }
  }
  items
}

/// A list as text, one entry per line.
pub fn list_text(items: &[String]) -> String {
  items.join("\n")
}

/// `https://host/...` without whitespace (plain `http` only for localhost, for development).
pub fn is_web_url(text: &str) -> bool {
  let Some((scheme, rest)) = text.split_once("://") else {
    return false;
  };
  let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
  let host_only = host.rsplit('@').next().unwrap_or_default();
  let is_local = host_only == "localhost" || host_only.starts_with("localhost:");
  (scheme == "https" || (scheme == "http" && is_local))
    && !host.contains('@')
    && !host_only.is_empty()
    && !text.chars().any(char::is_whitespace)
}

/// A lower-case DNS name such as `chat.example.com`.
pub fn is_dns_name(text: &str) -> bool {
  text.len() <= MAX_HOSTNAME_CHARS
    && text.split('.').all(|label| {
      !label.is_empty()
        && label.len() <= MAX_LABEL_CHARS
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
          .bytes()
          .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    })
}

/// An e-mail domain: a DNS name with at least one dot.
pub fn is_email_domain(text: &str) -> bool {
  text.contains('.') && is_dns_name(text)
}

/// `name@domain`, no whitespace, a dotted domain.
pub fn is_email_address(text: &str) -> bool {
  let Some((local, domain)) = text.split_once('@') else {
    return false;
  };
  !local.is_empty()
    && !text.chars().any(char::is_whitespace)
    && !domain.contains('@')
    && is_email_domain(&domain.to_ascii_lowercase())
}

/// A number typed into a field; blanks and garbage are `None`.
pub fn parse_number<T: std::str::FromStr>(text: &str) -> Option<T> {
  text.trim().parse().ok()
}

/// A raw value shown in a larger unit (`scale` raw units per shown unit), without trailing zeros.
pub fn format_scaled(raw: u64, scale: u64) -> String {
  if scale <= 1 {
    return raw.to_string();
  }
  let whole = raw / scale;
  let rest = raw % scale;
  if rest == 0 {
    return whole.to_string();
  }
  let value = raw as f64 / scale as f64;
  let text = format!("{value:.3}");
  text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// The raw value of a number typed in a larger unit; none for negative, non-finite or huge numbers.
pub fn parse_scaled(text: &str, scale: u64) -> Option<u64> {
  let value: f64 = text.trim().replace(',', ".").parse().ok()?;
  let raw = (value * scale.max(1) as f64).round();
  (value.is_finite() && value >= 0.0 && raw < 9.0e15).then_some(raw as u64)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn lists_split_on_commas_and_lines_and_drop_repeats() {
    assert_eq!(
      parse_list(" a.example.com, b.example.com;\nc.example.com\r\n,a.example.com ,, "),
      ["a.example.com", "b.example.com", "c.example.com"]
    );
    assert!(parse_list("  \n ,").is_empty());
    assert_eq!(list_text(&["a".to_owned(), "b".to_owned()]), "a\nb");
  }

  #[test]
  fn urls_must_be_https_without_credentials() {
    assert!(is_web_url("https://support.example.com/help?x=1"));
    assert!(is_web_url("http://localhost:8080/docs"));
    for bad in [
      "http://example.com",
      "https://",
      "https://user:pw@example.com",
      "javascript:alert(1)",
      "https://example.com/a b",
      "example.com",
    ] {
      assert!(!is_web_url(bad), "{bad}");
    }
  }

  #[test]
  fn dns_names_and_addresses_are_checked() {
    assert!(is_dns_name("chat.example.com"));
    assert!(is_dns_name("a-b.example.com"));
    for bad in [
      "",
      "Chat.example.com",
      "-a.example.com",
      "a..com",
      "a_b.com",
      "a b.com",
    ] {
      assert!(!is_dns_name(bad), "{bad:?}");
    }
    assert!(is_email_domain("example.com"));
    assert!(!is_email_domain("localhost"));
    assert!(is_email_address("noreply@example.com"));
    for bad in [
      "noreply",
      "@example.com",
      "a@b",
      "a b@example.com",
      "a@@example.com",
    ] {
      assert!(!is_email_address(bad), "{bad:?}");
    }
  }

  #[test]
  fn scaled_numbers_round_trip() {
    assert_eq!(format_scaled(120, 60), "2");
    assert_eq!(format_scaled(90, 60), "1.5");
    assert_eq!(format_scaled(45, 1), "45");
    assert_eq!(format_scaled(100 * 1_048_576, 1_048_576), "100");
    assert_eq!(parse_scaled("1.5", 60), Some(90));
    assert_eq!(parse_scaled("1,5", 60), Some(90));
    assert_eq!(parse_scaled(" 7 ", 1), Some(7));
    assert_eq!(parse_scaled("-1", 1), None);
    assert_eq!(parse_scaled("many", 1), None);
    assert_eq!(parse_scaled("", 1), None);
    assert_eq!(parse_scaled("1e30", 1), None);
  }

  #[test]
  fn numbers_parse_or_fail() {
    assert_eq!(parse_number::<u16>(" 587 "), Some(587));
    assert_eq!(parse_number::<u16>("70000"), None);
    assert_eq!(parse_number::<u16>(""), None);
  }

  #[test]
  fn every_label_is_translated() {
    use crate::i18n::{Locale, translate};

    for locale in [Locale::En, Locale::De] {
      for key in ["admin-source-config", "admin-source-database"] {
        assert_ne!(translate(locale, key, None), key, "{locale:?} lacks {key}");
      }
    }
  }
}
