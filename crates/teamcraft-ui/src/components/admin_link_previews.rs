//! The link preview part of the security policy (e2ee.md §12, issue #2107): whether messages show
//! previews of the links in them, and for which sites. Saved with the rest of the signed policy;
//! every member's client enforces it, as sender and as receiver.

use leptos::prelude::*;
use teamcraft_proto::{
  LinkPreviewMode, LinkPreviewPolicy, LinkPreviewPolicyError, MAX_LINK_PREVIEW_DOMAINS, WorkspacePolicy,
  normalize_link_preview_domain,
};

use crate::components::admin_policy::PolicyDraft;
use crate::form_errors::{FieldError, FieldErrors, FormErrors};
use crate::i18n::{I18n, use_i18n};

/// The id of the domain list, where its errors go.
pub const DOMAINS_FIELD: &str = "policy-link-preview-domains";
const HINT_ID: &str = "policy-link-preview-domains-hint";

/// The four choices of the form. `All` and `AllExcept` are both a block list on the wire; they
/// differ in whether the console offers the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Choice {
  Off,
  All,
  AllExcept,
  OnlyListed,
}

const CHOICES: [Choice; 4] = [Choice::Off, Choice::All, Choice::AllExcept, Choice::OnlyListed];

/// The link preview group of the security form. `errors` are the form's: the domain list's error
/// is shown at the list, and the form's submit checks it with [`link_preview_errors`].
#[component]
pub fn LinkPreviewGroup(policy: PolicyDraft, errors: FormErrors) -> impl IntoView {
  let i18n = use_i18n();
  let draft = policy.draft;
  let initial = draft.get_untracked().link_previews;
  // Kept apart from the draft: the list survives switching to a choice that doesn't use it, and the
  // text keeps what the admin typed, wrong lines included, until it is fixed.
  let choice = RwSignal::new(choice_of(&initial));
  let text = RwSignal::new(initial.domains.join("\n"));

  // The draft changed elsewhere (reset, a newer policy from another device): show it.
  Effect::new(move |_| {
    let current = draft.with(|policy| policy.link_previews.clone());
    let shown = link_previews_of(choice.get_untracked(), &text.get_untracked());
    if current != shown {
      choice.set(choice_of(&current));
      text.set(current.domains.join("\n"));
    }
  });
  // After a refused save the error follows the input (forms.md): the security form's own effect
  // keeps all of its fields' errors, this one's with them.

  let set = move |next_choice: Choice, next_text: String| {
    choice.set(next_choice);
    text.set(next_text.clone());
    let link_previews = link_previews_of(next_choice, &next_text);
    policy.edit(|p| p.link_previews = link_previews);
  };
  let is_list_unused = move || !uses_list(choice.get());

  view! {
    <fieldset class="policy-group" prop:disabled=move || policy.is_locked()>
      <legend>{move || i18n.t("policy-link-previews")}</legend>
      <p class="muted-text small">{move || i18n.t("policy-link-previews-hint")}</p>
      {CHOICES
        .into_iter()
        .map(|option| {
          let slug = slug(option);
          let label_key = format!("policy-link-previews-{slug}");
          let hint_key = format!("policy-link-previews-{slug}-hint");
          view! {
            <label class="radio">
              <input
                type="radio"
                name="link_previews"
                value=slug
                prop:checked=move || choice.get() == option
                on:change=move |_| set(option, text.get_untracked())
              />
              <span>
                <strong>{move || i18n.t(&label_key)}</strong>
                <span class="muted-text small">{move || i18n.t(&hint_key)}</span>
              </span>
            </label>
          }
        })
        .collect_view()}
      <div class="field">
        <label for=DOMAINS_FIELD>{move || i18n.t("policy-link-previews-domains")}</label>
        <textarea
          id=DOMAINS_FIELD
          class="input mono"
          name="link_preview_domains"
          rows="5"
          autocomplete="off"
          spellcheck="false"
          autocapitalize="off"
          placeholder="example.com"
          aria-invalid=errors.invalid(DOMAINS_FIELD)
          aria-describedby=errors.described_or(DOMAINS_FIELD, Some(HINT_ID))
          prop:disabled=is_list_unused
          prop:value=move || text.get()
          on:input=move |ev| set(choice.get_untracked(), event_target_value(&ev))
        ></textarea>
        <FieldError errors=errors field=DOMAINS_FIELD />
        <p class="muted-text small" id=HINT_ID>
          {move || {
            i18n.t_with("policy-link-previews-domains-hint", &[("max", &MAX_LINK_PREVIEW_DOMAINS.to_string())])
          }}
        </p>
      </div>
    </fieldset>
  }
}

/// The errors of the link preview part of `policy`, with the same rules the server and every
/// client apply ([`LinkPreviewPolicy::check`]), worded for the admin.
pub fn link_preview_errors(policy: &WorkspacePolicy, i18n: I18n) -> FieldErrors {
  let link_previews = &policy.link_previews;
  let mut found = FieldErrors::new();
  if let Err(error) = link_previews.check() {
    let message = match error {
      LinkPreviewPolicyError::TooManyDomains => {
        i18n.t_count("policy-link-previews-error-too-many", MAX_LINK_PREVIEW_DOMAINS)
      },
      LinkPreviewPolicyError::InvalidDomain(index) => {
        let entry = link_previews.domains.get(index).map_or("", String::as_str);
        i18n.t_with("policy-link-previews-error-invalid", &[("entry", entry)])
      },
      LinkPreviewPolicyError::EmptyAllowList => i18n.t("policy-link-previews-error-empty"),
    };
    found.push(DOMAINS_FIELD, message);
  }
  found
}

fn slug(choice: Choice) -> &'static str {
  match choice {
    Choice::Off => "off",
    Choice::All => "all",
    Choice::AllExcept => "except",
    Choice::OnlyListed => "only",
  }
}

fn uses_list(choice: Choice) -> bool {
  matches!(choice, Choice::AllExcept | Choice::OnlyListed)
}

/// The choice a stored policy shows as. Modes from a newer console show as off, which is what this
/// client does with them.
fn choice_of(policy: &LinkPreviewPolicy) -> Choice {
  match policy.mode {
    LinkPreviewMode::AllowList => Choice::OnlyListed,
    LinkPreviewMode::BlockList if policy.domains.is_empty() => Choice::All,
    LinkPreviewMode::BlockList => Choice::AllExcept,
    LinkPreviewMode::Off | LinkPreviewMode::Unknown(_) => Choice::Off,
  }
}

/// The policy the form says: the list only for the choices that use it.
fn link_previews_of(choice: Choice, text: &str) -> LinkPreviewPolicy {
  let mode = match choice {
    Choice::Off => LinkPreviewMode::Off,
    Choice::All | Choice::AllExcept => LinkPreviewMode::BlockList,
    Choice::OnlyListed => LinkPreviewMode::AllowList,
  };
  let domains = if uses_list(choice) {
    parse_domain_lines(text)
  } else {
    Vec::new()
  };
  LinkPreviewPolicy { mode, domains }
}

/// One entry per line (commas work too), each in its stored form; a line that isn't a host name is
/// kept as typed, so the check names it. Repeated entries count once.
fn parse_domain_lines(text: &str) -> Vec<String> {
  let mut domains: Vec<String> = Vec::new();
  for line in text.split(['\n', ',']) {
    let line = line.trim();
    if line.is_empty() {
      continue;
    }
    let entry = normalize_link_preview_domain(line).unwrap_or_else(|| line.to_owned());
    if !domains.contains(&entry) {
      domains.push(entry);
    }
  }
  domains
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn lines_become_stored_entries_and_bad_lines_stay_as_typed() {
    assert_eq!(
      parse_domain_lines(
        " Example.com\n\n*.docs.example.org ,bücher.example\nexample.com.\nhttps://x.example\n"
      ),
      vec![
        "example.com",
        "docs.example.org",
        "xn--bcher-kva.example",
        "https://x.example"
      ]
    );
    assert!(parse_domain_lines(" \n , ").is_empty());
  }

  #[test]
  fn choices_round_trip_through_the_policy() {
    for choice in CHOICES {
      let policy = link_previews_of(choice, "example.com");
      assert_eq!(choice_of(&policy), choice, "{choice:?}");
    }
    // The list is kept only where it is used.
    assert!(link_previews_of(Choice::All, "example.com").domains.is_empty());
    assert!(link_previews_of(Choice::Off, "example.com").domains.is_empty());
    // A block list without entries is "all sites".
    assert_eq!(choice_of(&link_previews_of(Choice::AllExcept, "")), Choice::All);
    assert_eq!(
      choice_of(&LinkPreviewPolicy {
        mode: LinkPreviewMode::Unknown("newer".into()),
        domains: Vec::new(),
      }),
      Choice::Off
    );
  }

  #[test]
  fn what_the_form_makes_passes_the_shared_check_once_the_lines_are_host_names() {
    assert_eq!(
      link_previews_of(Choice::OnlyListed, "Docs.Example.com\n*.example.org").check(),
      Ok(())
    );
    assert_eq!(
      link_previews_of(Choice::OnlyListed, "example.com\nnot a host").check(),
      Err(LinkPreviewPolicyError::InvalidDomain(1))
    );
    assert_eq!(
      link_previews_of(Choice::OnlyListed, "").check(),
      Err(LinkPreviewPolicyError::EmptyAllowList)
    );
  }
}
