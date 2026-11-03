use leptos::prelude::*;
use teamcraft_client_api::updates::ClientVersion;
use teamcraft_proto::{ClientUpdateMode, ClientUpdatePolicy, UpdateChannel};

use crate::components::admin_console::SectionHeader;
use crate::components::admin_policy::{PolicyBlockerNote, PolicyFooter, SelectRow, use_policy_draft};
use crate::form_errors::error_id;
use crate::i18n::use_i18n;

/// Desktop updates (docs/updates.md §4.4): part of the signed policy, so every desktop app follows
/// it and a server can't change it.
#[component]
pub fn UpdatesPolicySection() -> impl IntoView {
  let i18n = use_i18n();
  let policy = use_policy_draft();
  let draft = policy.draft;
  let updates = move || draft.get().client_updates;

  let channel_options = Signal::derive(move || {
    [
      (UpdateChannel::Stable, "policy-updates-channel-stable"),
      (UpdateChannel::Prerelease, "policy-updates-channel-prerelease"),
    ]
    .into_iter()
    .map(|(channel, key)| (channel.as_str().to_owned(), i18n.t(key)))
    .collect()
  });

  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-updates-title"))
      subtitle=Signal::derive(move || i18n.t("admin-updates-subtitle"))
    />
    <PolicyBlockerNote policy=policy />
    <form
      class="policy-form"
      on:submit=move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        policy.save();
      }
    >
      <fieldset class="policy-group" prop:disabled=move || policy.is_locked()>
        <legend>{move || i18n.t("policy-updates-mode")}</legend>
        {[ClientUpdateMode::Allowed, ClientUpdateMode::Disabled]
          .into_iter()
          .map(|mode| {
            let slug = mode.as_str().to_owned();
            let label_key = format!("policy-updates-{slug}");
            let hint_key = format!("policy-updates-{slug}-hint");
            let checked_mode = mode.clone();
            view! {
              <label class="radio">
                <input
                  type="radio"
                  name="client_update_mode"
                  value=slug
                  prop:checked=move || updates().mode == checked_mode
                  on:change=move |_| policy.edit(|value| value.client_updates.mode = mode.clone())
                />
                <span>
                  <strong>{move || i18n.t(&label_key)}</strong>
                  <span class="muted-text small">{move || i18n.t(&hint_key)}</span>
                </span>
              </label>
            }
          })
          .collect_view()}
      </fieldset>
      <fieldset class="policy-group" prop:disabled=move || policy.is_locked()>
        <legend>{move || i18n.t("policy-updates-releases")}</legend>
        <SelectRow
          id="policy-update-channel"
          label=Signal::derive(move || i18n.t("policy-updates-channel"))
          options=channel_options
          selected=Signal::derive(move || updates().channel.as_str().to_owned())
          on_select=Callback::new(move |value: String| {
            policy.edit(|policy| policy.client_updates.channel = UpdateChannel::from(value));
          })
        />
        <TextRow
          id="policy-update-pinned"
          label="policy-updates-pinned"
          hint="policy-updates-pinned-hint"
          placeholder="1.2.3"
          value=Signal::derive(move || updates().maybe_pinned_version.unwrap_or_default())
          normalize=version_text
          error="policy-updates-invalid-version"
          on_change=Callback::new(move |value: Option<String>| {
            policy.edit(|policy| policy.client_updates.maybe_pinned_version = value);
          })
        />
        <TextRow
          id="policy-update-minimum"
          label="policy-updates-minimum"
          hint="policy-updates-minimum-hint"
          placeholder="1.2.0"
          value=Signal::derive(move || updates().maybe_minimum_version.unwrap_or_default())
          normalize=version_text
          error="policy-updates-invalid-version"
          on_change=Callback::new(move |value: Option<String>| {
            policy.edit(|policy| policy.client_updates.maybe_minimum_version = value);
          })
        />
        <TextRow
          id="policy-update-feed"
          label="policy-updates-feed"
          hint="policy-updates-feed-hint"
          placeholder="https://updates.example.com/teamcraft/latest.json"
          value=Signal::derive(move || updates().maybe_feed_url.unwrap_or_default())
          normalize=feed_url
          error="policy-updates-invalid-url"
          on_change=Callback::new(move |value: Option<String>| {
            policy.edit(|policy| policy.client_updates.maybe_feed_url = value);
          })
        />
        <button
          type="button"
          class="btn"
          disabled=move || updates() == ClientUpdatePolicy::default()
          on:click=move |_| policy.edit(|policy| policy.client_updates = ClientUpdatePolicy::default())
        >
          {move || i18n.t("policy-updates-defaults")}
        </button>
      </fieldset>
      <PolicyFooter policy=policy />
    </form>
  }
}

/// A text field that writes into the draft only when the text is empty (`None`) or `normalize`
/// accepts it; otherwise it says what is wrong and leaves the draft alone.
#[component]
fn TextRow(
  id: &'static str,
  label: &'static str,
  hint: &'static str,
  placeholder: &'static str,
  #[prop(into)] value: Signal<String>,
  normalize: fn(&str) -> Option<String>,
  error: &'static str,
  on_change: Callback<Option<String>>,
) -> impl IntoView {
  let i18n = use_i18n();
  let is_invalid = RwSignal::new(false);
  let hint_id = format!("{id}-hint");
  let message_id = error_id(id);
  let described = {
    let (hint_id, message_id) = (hint_id.clone(), message_id.clone());
    move || {
      if is_invalid.get() {
        format!("{hint_id} {message_id}")
      } else {
        hint_id.clone()
      }
    }
  };
  view! {
    <div class="field">
      <label for=id>{move || i18n.t(label)}</label>
      <input
        id=id
        class="input"
        translate="no"
        placeholder=placeholder
        aria-describedby=described
        aria-invalid=move || is_invalid.get().then_some("true")
        prop:value=move || value.get()
        on:change=move |ev| {
          let text = event_target_value(&ev);
          match parse_optional(&text, normalize) {
            Ok(maybe_value) => {
              is_invalid.set(false);
              on_change.run(maybe_value);
            },
            Err(()) => is_invalid.set(true),
          }
        }
      />
      <p id=hint_id class="muted-text small">{move || i18n.t(hint)}</p>
      <p id=message_id class="field-error">{move || is_invalid.get().then(|| i18n.t(error))}</p>
    </div>
  }
}

/// Empty text clears the setting; anything else must pass `normalize`.
fn parse_optional(text: &str, normalize: fn(&str) -> Option<String>) -> Result<Option<String>, ()> {
  let trimmed = text.trim();
  if trimmed.is_empty() {
    Ok(None)
  } else {
    normalize(trimmed).map(Some).ok_or(())
  }
}

/// `v1.2.3` is stored as `1.2.3`.
fn version_text(text: &str) -> Option<String> {
  ClientVersion::parse(text).ok().map(|version| version.to_string())
}

fn feed_url(text: &str) -> Option<String> {
  (text.len() > "https://".len() && text.starts_with("https://") && !text.contains(char::is_whitespace))
    .then(|| text.to_owned())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn versions_are_stored_without_the_v_and_empty_text_clears() {
    assert_eq!(
      parse_optional(" v1.2.3 ", version_text),
      Ok(Some("1.2.3".to_owned()))
    );
    assert_eq!(
      parse_optional("1.2.0-rc.1", version_text),
      Ok(Some("1.2.0-rc.1".to_owned()))
    );
    assert_eq!(parse_optional("   ", version_text), Ok(None));
    assert_eq!(parse_optional("soon", version_text), Err(()));
  }

  #[test]
  fn feed_urls_must_be_https() {
    assert!(feed_url("https://updates.example.com/latest.json").is_some());
    assert!(feed_url("http://updates.example.com/latest.json").is_none());
    assert!(feed_url("https://").is_none());
    assert!(feed_url("https://example.com/a b").is_none());
    assert_eq!(
      parse_optional("https://updates.example.com/latest.json", feed_url),
      Ok(Some("https://updates.example.com/latest.json".to_owned()))
    );
  }

  #[test]
  fn every_label_is_translated() {
    use crate::i18n::{Locale, translate};

    let keys = [
      "admin-updates-title",
      "admin-updates-subtitle",
      "policy-updates-mode",
      "policy-updates-allowed",
      "policy-updates-allowed-hint",
      "policy-updates-disabled",
      "policy-updates-disabled-hint",
      "policy-updates-releases",
      "policy-updates-channel",
      "policy-updates-channel-stable",
      "policy-updates-channel-prerelease",
      "policy-updates-pinned",
      "policy-updates-pinned-hint",
      "policy-updates-minimum",
      "policy-updates-minimum-hint",
      "policy-updates-feed",
      "policy-updates-feed-hint",
      "policy-updates-invalid-version",
      "policy-updates-invalid-url",
      "policy-updates-defaults",
    ];
    for locale in [Locale::En, Locale::De] {
      for key in keys {
        assert_ne!(translate(locale, key, None), key, "{locale:?} lacks {key}");
      }
    }
  }
}
