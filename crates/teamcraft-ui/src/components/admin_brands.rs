use std::collections::BTreeMap;

use leptos::prelude::*;
use teamcraft_proto::{
  AdminBrand, AdminBrandsResponse, BrandAssetSlot, BrandColors, BrandLinks, BrandMailSettings, BrandSettings,
  BrandSource, CreateBrandRequest, ServedHostname, UpdateBrandRequest, brand_color_properties,
  is_brand_hex_color,
};
use wasm_bindgen_futures::JsFuture;

use crate::admin::format_bytes;
use crate::components::admin_console::SectionHeader;
use crate::components::admin_fields::{
  Bound, CheckField, FieldErrors, TextAreaField, TextField, Validated, bind, bind_bool, is_dns_name,
  is_email_address, is_email_domain, is_web_url, list_text, parse_list,
};
use crate::i18n::use_i18n;
use crate::reactive::SetIfAlive;
use crate::state::use_app_state;

/// Below this contrast ratio (WCAG AA for normal text) a color pair is flagged.
const MIN_CONTRAST: f64 = 4.5;
const MAX_NAME_CHARS: usize = 100;
const MAX_TEXT_CHARS: usize = 1000;
const MAX_ID_CHARS: usize = 32;
/// Text colors the interface uses on an accent when the brand sets no `on_accent`.
const LIGHT_TEXT: [u8; 3] = [255, 255, 255];
const DARK_TEXT: [u8; 3] = [26, 29, 33];
/// Brand colors in the order of the form; the keys the server knows.
const COLOR_KEYS: [&str; 7] = [
  "accent",
  "accent_strong",
  "on_accent",
  "accent_soft",
  "sidebar",
  "sidebar_current",
  "chrome",
];
const COLOR_FIELDS: [[&str; 7]; 2] = [
  [
    "color_light_accent",
    "color_light_accent_strong",
    "color_light_on_accent",
    "color_light_accent_soft",
    "color_light_sidebar",
    "color_light_sidebar_current",
    "color_light_chrome",
  ],
  [
    "color_dark_accent",
    "color_dark_accent_strong",
    "color_dark_on_accent",
    "color_dark_accent_soft",
    "color_dark_sidebar",
    "color_dark_sidebar_current",
    "color_dark_chrome",
  ],
];
const SLOTS: [(&str, BrandAssetSlot); 7] = [
  ("admin-brands-slot-logo", BrandAssetSlot::Logo),
  ("admin-brands-slot-logo-dark", BrandAssetSlot::LogoDark),
  ("admin-brands-slot-icon", BrandAssetSlot::Icon),
  ("admin-brands-slot-icon-dark", BrandAssetSlot::IconDark),
  ("admin-brands-slot-favicon", BrandAssetSlot::Favicon),
  ("admin-brands-slot-favicon-dark", BrandAssetSlot::FaviconDark),
  ("admin-brands-slot-sso-icon", BrandAssetSlot::SsoIcon),
];
/// Longest provider name on the sign-in button.
const MAX_SSO_LABEL_CHARS: usize = 64;
const IMAGE_TYPES: [&str; 6] = [
  "image/png",
  "image/jpeg",
  "image/webp",
  "image/svg+xml",
  "image/x-icon",
  "image/vnd.microsoft.icon",
];

/// What the editor below the list shows.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Selection {
  Nothing,
  New,
  Existing(String),
}

/// Brands (docs/branding.md): host names, colors, texts, links and images per brand. Brands of the
/// configuration file are read-only until taken over into the database.
#[component]
pub fn BrandsSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let response = Memo::new(move |_| state.read(|backend| backend.admin_brands()));
  let selected = RwSignal::new(Selection::Nothing);
  // The editor is rebuilt when the selection changes or the selected brand changes its source (a
  // take-over), never on a plain refresh, so edits in progress survive.
  let editor_key = Memo::new(move |_| match selected.get() {
    Selection::Existing(id) => {
      let source = response
        .get()
        .and_then(|response| response.brands.into_iter().find(|brand| brand.id == id))
        .map(|brand| brand.source);
      (Selection::Existing(id), source)
    },
    other => (other, None),
  });

  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-brands-title"))
      subtitle=Signal::derive(move || i18n.t("admin-brands-subtitle"))
    />
    {move || {
      response
        .get()
        .is_none()
        .then(|| view! { <p class="muted-text" role="status">{i18n.t("admin-settings-loading")}</p> })
    }}
    <div class="policy-form">
      <section class="policy-group" aria-label=move || i18n.t("admin-brands-list")>
        <h3>{move || i18n.t("admin-brands-list")}</h3>
        <ul class="admin-list">
          {move || {
            let Some(response) = response.get() else {
              return Vec::new();
            };
            response
              .brands
              .iter()
              .map(|brand| brand_row(brand, &response, selected))
              .collect::<Vec<_>>()
          }}
        </ul>
        <button type="button" class="btn" on:click=move |_| selected.set(Selection::New)>
          {move || i18n.t("admin-brands-new")}
        </button>
      </section>
      {move || {
        let (selection, _) = editor_key.get();
        let maybe_response = response.get_untracked();
        match (selection, maybe_response) {
          (Selection::Nothing, _) | (_, None) => ().into_any(),
          (Selection::New, Some(_)) => view! { <BrandEditor maybe_brand=None selected=selected response=response /> }.into_any(),
          (Selection::Existing(id), Some(snapshot)) => {
            match snapshot.brands.into_iter().find(|brand| brand.id == id) {
              Some(brand) => {
                view! { <BrandEditor maybe_brand=Some(brand) selected=selected response=response /> }.into_any()
              },
              None => ().into_any(),
            }
          },
        }
      }}
    </div>
  }
}

fn brand_row(
  brand: &AdminBrand,
  response: &AdminBrandsResponse,
  selected: RwSignal<Selection>,
) -> impl IntoView + use<> {
  let i18n = use_i18n();
  let id = brand.id.clone();
  let active_hosts: Vec<String> = response
    .served_hostnames
    .iter()
    .filter(|served| served.maybe_brand_id.as_deref() == Some(&brand.id))
    .map(|served| served.hostname.clone())
    .collect();
  let is_read_only = brand.source != BrandSource::Database;
  let source_key = source_key(&brand.source);
  view! {
    <li class="admin-row brand-row">
      <span class="admin-row-name">
        <strong>{brand.settings.display_name.clone()}</strong>
        <span class="muted-text small" translate="no">{format!(" {}", brand.id)}</span>
        <br />
        <span class="muted-text small" translate="no">{active_hosts.join(", ")}</span>
      </span>
      <span class="badge">{move || i18n.t(source_key)}</span>
      {brand.is_default.then(|| view! { <span class="badge badge-active">{move || i18n.t("admin-brands-default")}</span> })}
      {brand
        .is_overriding_config
        .then(|| view! { <span class="badge badge-warn">{move || i18n.t("admin-brands-overrides")}</span> })}
      <button type="button" class="btn btn-small" on:click=move |_| selected.set(Selection::Existing(id.clone()))>
        {move || i18n.t(if is_read_only { "admin-brands-view" } else { "admin-brands-edit" })}
      </button>
    </li>
  }
}

fn source_key(source: &BrandSource) -> &'static str {
  match source {
    BrandSource::Builtin => "admin-brands-source-builtin",
    BrandSource::Database => "admin-brands-source-database",
    _ => "admin-brands-source-config",
  }
}

/// The form of one brand (or a new one). Brands of the file and the built-in brand are read-only.
#[component]
fn BrandEditor(
  maybe_brand: Option<AdminBrand>,
  selected: RwSignal<Selection>,
  response: Memo<Option<AdminBrandsResponse>>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let is_new = maybe_brand.is_none();
  let source = maybe_brand
    .as_ref()
    .map_or(BrandSource::Database, |brand| brand.source.clone());
  let is_read_only = source != BrandSource::Database;
  let id = maybe_brand
    .as_ref()
    .map(|brand| brand.id.clone())
    .unwrap_or_default();
  let draft = RwSignal::new(match &maybe_brand {
    Some(brand) => BrandDraft::from_brand(brand),
    None => BrandDraft::new_brand(),
  });
  let show_errors = RwSignal::new(false);
  let is_confirming_delete = RwSignal::new(false);

  let new_id = bind(draft, show_errors, "id", |d| &d.new_id, |d| &mut d.new_id);
  let display_name = bind(
    draft,
    show_errors,
    "display_name",
    |d| &d.display_name,
    |d| &mut d.display_name,
  );
  let organization = bind(
    draft,
    show_errors,
    "organization_name",
    |d| &d.organization_name,
    |d| &mut d.organization_name,
  );
  let sso_label = bind(
    draft,
    show_errors,
    "sso_label",
    |d| &d.sso_label,
    |d| &mut d.sso_label,
  );
  let email_domains = bind(
    draft,
    show_errors,
    "email_domains",
    |d| &d.email_domains,
    |d| &mut d.email_domains,
  );
  let title_en = bind(
    draft,
    show_errors,
    "title_en",
    |d| &d.title_en,
    |d| &mut d.title_en,
  );
  let title_de = bind(
    draft,
    show_errors,
    "title_de",
    |d| &d.title_de,
    |d| &mut d.title_de,
  );
  let text_en = bind(draft, show_errors, "text_en", |d| &d.text_en, |d| &mut d.text_en);
  let text_de = bind(draft, show_errors, "text_de", |d| &d.text_de, |d| &mut d.text_de);
  let support = bind(draft, show_errors, "support", |d| &d.support, |d| &mut d.support);
  let privacy = bind(draft, show_errors, "privacy", |d| &d.privacy, |d| &mut d.privacy);
  let terms = bind(draft, show_errors, "terms", |d| &d.terms, |d| &mut d.terms);
  let imprint = bind(draft, show_errors, "imprint", |d| &d.imprint, |d| &mut d.imprint);
  let download = bind(
    draft,
    show_errors,
    "download",
    |d| &d.download,
    |d| &mut d.download,
  );
  let sender_name = bind(
    draft,
    show_errors,
    "sender_name",
    |d| &d.sender_name,
    |d| &mut d.sender_name,
  );
  let from_address = bind(
    draft,
    show_errors,
    "from_address",
    |d| &d.from_address,
    |d| &mut d.from_address,
  );
  let (is_default, set_default) = bind_bool(draft, |d| d.is_default, |d, value| d.is_default = value);

  let save_id = id.clone();
  let save = move |ev: web_sys::SubmitEvent| {
    ev.prevent_default();
    show_errors.set(true);
    let Some((settings, brand_id, is_default)) = draft.with_untracked(|draft| {
      draft.errors().is_empty().then(|| {
        (
          draft.to_settings(),
          draft.new_id.trim().to_owned(),
          draft.is_default,
        )
      })
    }) else {
      return;
    };
    if is_new {
      let request = CreateBrandRequest {
        id: brand_id,
        is_default,
        settings,
      };
      state.write(|backend| backend.create_brand(request));
      selected.set(Selection::Nothing);
    } else {
      let request = UpdateBrandRequest { is_default, settings };
      let id = save_id.clone();
      state.write(|backend| backend.update_brand(id, request));
    }
  };
  let delete_id = id.clone();
  let take_over_id = id.clone();
  let title = if is_new {
    "admin-brands-new".to_owned()
  } else if is_read_only {
    "admin-brands-view".to_owned()
  } else {
    "admin-brands-edit".to_owned()
  };
  let can_delete = !is_new && !is_read_only && !maybe_brand.as_ref().is_some_and(|brand| brand.is_default);
  let can_take_over = source == BrandSource::Config;
  let overrides_config = maybe_brand
    .as_ref()
    .is_some_and(|brand| brand.is_overriding_config);
  let delete_label = if overrides_config {
    "admin-brands-delete-restore"
  } else {
    "admin-brands-delete"
  };
  let colors_assets_id = id.clone();

  view! {
    <form class="brand-editor" on:submit=save>
      <fieldset class="policy-group" prop:disabled=is_read_only>
        <legend>{move || i18n.t(&title)}</legend>
        {is_read_only
          .then(|| {
            let key = if source == BrandSource::Builtin {
              "admin-brands-builtin-note"
            } else {
              "admin-brands-config-note"
            };
            view! { <p class="muted-text small" role="status">{move || i18n.t(key)}</p> }
          })}
        {is_new
          .then(|| view! {
            <TextField
              id="brand-id"
              label="admin-brands-id"
              hint="admin-brands-id-hint"
              placeholder="brand-a"
              value=new_id.value
              on_input=new_id.on_input
              error=new_id.error
            />
          })}
        <TextField
          id="brand-display-name"
          label="admin-brands-display-name"
          value=display_name.value
          on_input=display_name.on_input
          error=display_name.error
        />
        <TextField
          id="brand-organization"
          label="admin-brands-organization"
          hint="admin-brands-organization-hint"
          value=organization.value
          on_input=organization.on_input
          error=organization.error
        />
        <TextField
          id="brand-sso-label"
          label="admin-brands-sso-label"
          hint="admin-brands-sso-label-hint"
          placeholder="Microsoft"
          value=sso_label.value
          on_input=sso_label.on_input
          error=sso_label.error
        />
        <CheckField
          label="admin-brands-is-default"
          hint="admin-brands-is-default-hint"
          checked=is_default
          on_change=set_default
        />
      </fieldset>
      <fieldset class="policy-group" prop:disabled=is_read_only>
        <legend>{move || i18n.t("admin-brands-hosts")}</legend>
        <p class="muted-text small">{move || i18n.t("admin-brands-hosts-hint")}</p>
        <HostnameChoices draft=draft response=response own_id=id.clone() />
        <TextAreaField
          id="brand-email-domains"
          label="admin-brands-email-domains"
          hint="admin-brands-email-domains-hint"
          rows=2
          value=email_domains.value
          on_input=email_domains.on_input
          error=email_domains.error
        />
      </fieldset>
      <fieldset class="policy-group" prop:disabled=is_read_only>
        <legend>{move || i18n.t("admin-brands-login")}</legend>
        <TextField id="brand-title-en" label="admin-brands-title-en" value=title_en.value on_input=title_en.on_input error=title_en.error />
        <TextAreaField id="brand-text-en" label="admin-brands-text-en" rows=2 value=text_en.value on_input=text_en.on_input error=text_en.error />
        <TextField id="brand-title-de" label="admin-brands-title-de" value=title_de.value on_input=title_de.on_input error=title_de.error />
        <TextAreaField id="brand-text-de" label="admin-brands-text-de" rows=2 value=text_de.value on_input=text_de.on_input error=text_de.error />
      </fieldset>
      <fieldset class="policy-group" prop:disabled=is_read_only>
        <legend>{move || i18n.t("admin-brands-links")}</legend>
        <LinkField id="brand-link-support" label="admin-brands-link-support" bound=support />
        <LinkField id="brand-link-privacy" label="admin-brands-link-privacy" bound=privacy />
        <LinkField id="brand-link-terms" label="admin-brands-link-terms" bound=terms />
        <LinkField id="brand-link-imprint" label="admin-brands-link-imprint" bound=imprint />
        <LinkField id="brand-link-download" label="admin-brands-link-download" bound=download />
      </fieldset>
      <fieldset class="policy-group" prop:disabled=is_read_only>
        <legend>{move || i18n.t("admin-brands-mail")}</legend>
        <p class="muted-text small">{move || i18n.t("admin-brands-mail-hint")}</p>
        <TextField id="brand-sender-name" label="admin-brands-sender-name" value=sender_name.value on_input=sender_name.on_input error=sender_name.error />
        <TextField
          id="brand-from-address"
          label="admin-brands-from-address"
          placeholder="chat@example.com"
          value=from_address.value
          on_input=from_address.on_input
          error=from_address.error
        />
      </fieldset>
      <fieldset class="policy-group" prop:disabled=is_read_only>
        <legend>{move || i18n.t("admin-brands-colors")}</legend>
        <p class="muted-text small">{move || i18n.t("admin-brands-colors-hint")}</p>
        <div class="brand-colors">
          <ColorColumn draft=draft show_errors=show_errors is_dark=false />
          <ColorColumn draft=draft show_errors=show_errors is_dark=true />
        </div>
      </fieldset>
      {(!is_new)
        .then(|| view! { <BrandAssets id=colors_assets_id.clone() response=response is_read_only=is_read_only /> })}
      <div class="policy-footer">
        {(!is_read_only).then(|| view! { <button type="submit" class="btn btn-primary">{move || i18n.t("admin-save")}</button> })}
        {can_take_over
          .then(|| {
            let id = take_over_id.clone();
            view! {
              <button
                type="button"
                class="btn btn-primary"
                on:click=move |_| state.write(|backend| backend.take_over_brand(id.clone()))
              >
                {move || i18n.t("admin-brands-take-over")}
              </button>
            }
          })}
        {can_delete
          .then(|| {
            let id = delete_id.clone();
            view! {
              <button
                type="button"
                class="btn btn-danger"
                on:click=move |_| {
                  if is_confirming_delete.get_untracked() {
                    let id = id.clone();
                    state.write(|backend| backend.delete_brand(id));
                    selected.set(Selection::Nothing);
                  } else {
                    is_confirming_delete.set(true);
                  }
                }
              >
                {move || {
                  if is_confirming_delete.get() { i18n.t("admin-brands-delete-confirm") } else { i18n.t(delete_label) }
                }}
              </button>
            }
          })}
        <button type="button" class="btn" on:click=move |_| selected.set(Selection::Nothing)>
          {move || i18n.t("admin-brands-close")}
        </button>
      </div>
      {can_take_over
        .then(|| view! { <p class="muted-text small">{move || i18n.t("admin-brands-take-over-hint")}</p> })}
    </form>
  }
}

#[component]
fn LinkField(id: &'static str, label: &'static str, bound: Bound) -> impl IntoView {
  view! {
    <TextField
      id=id
      label=label
      placeholder="https://example.com/"
      input_type="url"
      value=bound.value
      on_input=bound.on_input
      error=bound.error
    />
  }
}

/// The host names the server is deployed for, as checkboxes, with the brand that uses each now.
#[component]
fn HostnameChoices(
  draft: RwSignal<BrandDraft>,
  response: Memo<Option<AdminBrandsResponse>>,
  own_id: String,
) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <ul class="admin-list brand-hosts">
      {move || {
        let Some(response) = response.get() else {
          return Vec::new();
        };
        let chosen = draft.with(|draft| draft.hostnames.clone());
        let mut rows: Vec<ServedHostname> = response.served_hostnames.clone();
        for hostname in &chosen {
          if !rows.iter().any(|served| &served.hostname == hostname) {
            rows.push(ServedHostname {
              hostname: hostname.clone(),
              maybe_brand_id: None,
            });
          }
        }
        let names: BTreeMap<String, String> = response
          .brands
          .iter()
          .map(|brand| (brand.id.clone(), brand.settings.display_name.clone()))
          .collect();
        rows
          .into_iter()
          .map(|served| {
            let is_served = response.served_hostnames.iter().any(|known| known.hostname == served.hostname);
            let state = hostname_state(&served, &own_id);
            let label = match state {
              HostState::Active => i18n.t("admin-brands-host-active"),
              HostState::UsedBy(ref other) => i18n.t_with(
                "admin-brands-host-used-by",
                &[("brand", names.get(other).map_or(other.as_str(), String::as_str))],
              ),
              HostState::Unused => i18n.t("admin-brands-host-unused"),
            };
            let hostname = served.hostname.clone();
            let toggle_name = served.hostname.clone();
            let is_checked = chosen.contains(&served.hostname);
            view! {
              <li class="admin-row">
                <label class="checkbox">
                  <input
                    type="checkbox"
                    prop:checked=is_checked
                    prop:disabled=move || !is_served && !is_checked
                    on:change=move |ev| {
                      let is_on = event_target_checked(&ev);
                      let name = toggle_name.clone();
                      draft.update(|draft| draft.set_hostname(&name, is_on));
                    }
                  />
                  <span translate="no">{hostname}</span>
                </label>
                <span class="muted-text small">
                  {if is_served { label } else { i18n.t("admin-brands-host-not-served") }}
                </span>
              </li>
            }
          })
          .collect::<Vec<_>>()
      }}
    </ul>
  }
}

/// The colors of one theme: a row per key, a live preview and the contrast check.
#[component]
fn ColorColumn(draft: RwSignal<BrandDraft>, show_errors: RwSignal<bool>, is_dark: bool) -> impl IntoView {
  let i18n = use_i18n();
  let heading = if is_dark {
    "admin-brands-colors-dark"
  } else {
    "admin-brands-colors-light"
  };
  let colors = Signal::derive(move || draft.with(|draft| draft.theme_colors(is_dark)));
  let preview = NodeRef::<leptos::html::Div>::new();
  // CSSOM properties, not a `style` attribute: the CSP (`style-src 'self'`) blocks those.
  let applied = StoredValue::new(Vec::<&'static str>::new());
  Effect::new(move || {
    let properties = preview_properties(&colors.get());
    let Some(element) = preview.get() else {
      return;
    };
    let style = web_sys::HtmlElement::style(&element);
    for property in applied.get_value() {
      drop(style.remove_property(property));
    }
    for (property, value) in &properties {
      drop(style.set_property(property, value));
    }
    applied.set_value(properties.into_iter().map(|(property, _)| property).collect());
  });
  let checks = Signal::derive(move || contrast_checks(&colors.get()));
  view! {
    <div class="brand-color-column">
      <h4>{move || i18n.t(heading)}</h4>
      {COLOR_KEYS
        .iter()
        .enumerate()
        .map(|(index, key)| {
          let id = format!("brand-color-{}-{key}", if is_dark { "dark" } else { "light" });
          let label = format!("admin-brands-color-{}", key.replace('_', "-"));
          let error_id = COLOR_FIELDS[usize::from(is_dark)][index];
          let value = Signal::derive(move || draft.with(|draft| draft.theme_colors(is_dark)[index].1.clone()));
          let error = Signal::derive(move || {
            if show_errors.get() {
              draft.with(|draft| {
                draft.errors().into_iter().find(|(field, _)| *field == error_id).map(|(_, key)| key)
              })
            } else {
              None
            }
          });
          let picker_value = Signal::derive(move || expand_hex(&value.get()).unwrap_or_else(|| "#000000".to_owned()));
          view! {
            <div class="field brand-color-field">
              <label for=id.clone()>{move || i18n.t(&label)}</label>
              <div class="field-row">
                <input
                  id=id.clone()
                  class="input"
                  translate="no"
                  placeholder="#0b7f78"
                  aria-invalid=move || error.get().map(|_| "true")
                  prop:value=move || value.get()
                  on:input=move |ev| {
                    let text = event_target_value(&ev);
                    draft.update(|draft| draft.set_color(is_dark, index, text));
                  }
                />
                <input
                  type="color"
                  class="brand-color-picker"
                  aria-label=move || i18n.t("admin-brands-color-picker")
                  prop:value=move || picker_value.get()
                  on:input=move |ev| {
                    let text = event_target_value(&ev);
                    draft.update(|draft| draft.set_color(is_dark, index, text));
                  }
                />
              </div>
              {move || error.get().map(|key| view! { <p class="field-error" role="alert">{i18n.t(key)}</p> })}
            </div>
          }
        })
        .collect_view()}
      <div class=if is_dark { "brand-preview is-dark" } else { "brand-preview" } node_ref=preview aria-hidden="true">
        <div class="brand-preview-chrome"></div>
        <div class="brand-preview-body">
          <div class="brand-preview-sidebar">
            <span class="brand-preview-current"></span>
            <span></span>
          </div>
          <div class="brand-preview-main">
            <span class="brand-preview-button">{move || i18n.t("admin-brands-preview-button")}</span>
            <span class="brand-preview-soft">{move || i18n.t("admin-brands-preview-soft")}</span>
          </div>
        </div>
      </div>
      <ul class="brand-contrast small" aria-live="polite">
        {move || {
          checks
            .get()
            .into_iter()
            .map(|check| {
              let key = match check.pair {
                ContrastPair::OnAccent => "admin-brands-contrast-on-accent",
                ContrastPair::DefaultText => "admin-brands-contrast-default-text",
              };
              let ratio = format!("{:.1}", check.ratio);
              let text = i18n.t_with(key, &[("ratio", &ratio)]);
              let class = if check.is_ok() { "muted-text" } else { "field-error" };
              view! {
                <li class=class>
                  {text}
                  {(!check.is_ok()).then(|| view! { <strong>{" "}{i18n.t("admin-brands-contrast-warning")}</strong> })}
                </li>
              }
            })
            .collect_view()
        }}
      </ul>
    </div>
  }
}

/// The images of a brand: preview, upload and delete per slot.
#[component]
fn BrandAssets(id: String, response: Memo<Option<AdminBrandsResponse>>, is_read_only: bool) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let brand_id = id.clone();
  let brand = Memo::new(move |_| {
    response
      .get()
      .and_then(|response| response.brands.into_iter().find(|brand| brand.id == brand_id))
  });
  let can_upload = Memo::new(move |_| {
    response
      .get()
      .is_some_and(|response| response.is_image_upload_available)
  });
  let max_bytes = Memo::new(move |_| response.get().map_or(0, |response| response.max_asset_bytes));
  let maybe_error = RwSignal::new(None::<&'static str>);
  view! {
    <fieldset class="policy-group" prop:disabled=is_read_only>
      <legend>{move || i18n.t("admin-brands-images")}</legend>
      <p class="muted-text small">
        {move || {
          if can_upload.get() {
            i18n.t_with("admin-brands-images-hint", &[("max", &format_bytes(max_bytes.get()))])
          } else {
            i18n.t("admin-brands-images-unavailable")
          }
        }}
      </p>
      <ul class="admin-list">
        {SLOTS
          .into_iter()
          .map(|(label, slot)| {
            let id = id.clone();
            let slot_for_view = slot.clone();
            let has_image = {
              let slot = slot.clone();
              Memo::new(move |_| brand.get().is_some_and(|brand| brand.assets.contains(&slot)))
            };
            let preview = {
              let (id, slot) = (id.clone(), slot.clone());
              Memo::new(move |_| {
                if has_image.get() {
                  state.read(|backend| backend.brand_asset_preview(&id, &slot))
                } else {
                  None
                }
              })
            };
            let upload_id = id.clone();
            let upload_slot = slot.clone();
            let delete_id = id.clone();
            let delete_slot = slot.clone();
            view! {
              <li class="admin-row brand-asset-row">
                <span class="brand-asset-preview" class:is-dark=slot_for_view.as_str().ends_with("_dark")>
                  {move || {
                    preview
                      .get()
                      .map(|url| view! { <img class="brand-asset-img" src=url alt=i18n.t(label) /> })
                  }}
                </span>
                <span class="admin-row-name">
                  {move || i18n.t(label)}
                  <br />
                  <span class="muted-text small">
                    {move || i18n.t(if has_image.get() { "admin-brands-image-set" } else { "admin-brands-image-none" })}
                  </span>
                </span>
                <input
                  type="file"
                  accept=IMAGE_TYPES.join(",")
                  aria-label=move || i18n.t(label)
                  prop:disabled=move || !can_upload.get()
                  on:change=move |ev| {
                    let input = event_target::<web_sys::HtmlInputElement>(&ev);
                    let Some(file) = input.files().and_then(|files| files.item(0)) else {
                      return;
                    };
                    let content_type = file.type_();
                    if let Err(key) = check_asset(&content_type, file.size() as u64, max_bytes.get_untracked()) {
                      maybe_error.set(Some(key));
                      input.set_value("");
                      return;
                    }
                    maybe_error.set(None);
                    let (id, slot) = (upload_id.clone(), upload_slot.clone());
                    leptos::task::spawn_local(async move {
                      match JsFuture::from(file.array_buffer()).await {
                        Ok(buffer) => {
                          let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
                          state.try_write(|backend| backend.upload_brand_asset(id, slot, bytes, content_type));
                        },
                        Err(_) => maybe_error.set_if_alive(Some("admin-error-image-unreadable")),
                      }
                      // async-ok: a DOM input element, not a signal.
                      input.set_value("");
                    });
                  }
                />
                <button
                  type="button"
                  class="btn btn-danger btn-small"
                  prop:disabled=move || !has_image.get()
                  on:click=move |_| {
                    let (id, slot) = (delete_id.clone(), delete_slot.clone());
                    state.write(|backend| backend.delete_brand_asset(id, slot));
                  }
                >
                  {move || i18n.t("admin-brands-image-delete")}
                </button>
              </li>
            }
          })
          .collect_view()}
      </ul>
      {move || maybe_error.get().map(|key| view! { <p class="field-error" role="alert">{i18n.t(key)}</p> })}
    </fieldset>
  }
}

// ---- State of the form (pure, tested) ------------------------------------------------------------

/// Whether a host name is in use by the brand being edited, by another brand, or by none.
#[derive(Clone, Debug, PartialEq, Eq)]
enum HostState {
  Active,
  UsedBy(String),
  Unused,
}

fn hostname_state(served: &ServedHostname, own_id: &str) -> HostState {
  match served.maybe_brand_id.as_deref() {
    Some(id) if id == own_id => HostState::Active,
    Some(other) => HostState::UsedBy(other.to_owned()),
    None => HostState::Unused,
  }
}

/// What the brand form holds, as typed.
#[derive(Clone, Debug, PartialEq, Eq)]
struct BrandDraft {
  is_new: bool,
  new_id: String,
  is_default: bool,
  display_name: String,
  organization_name: String,
  /// The provider part of "Sign in with …".
  sso_label: String,
  hostnames: Vec<String>,
  email_domains: String,
  title_en: String,
  title_de: String,
  text_en: String,
  text_de: String,
  /// Texts in other languages, kept as they are.
  other_titles: BTreeMap<String, String>,
  other_texts: BTreeMap<String, String>,
  support: String,
  privacy: String,
  terms: String,
  imprint: String,
  download: String,
  sender_name: String,
  from_address: String,
  light: [String; 7],
  dark: [String; 7],
}

impl BrandDraft {
  fn new_brand() -> Self {
    Self::from_settings(&BrandSettings::default(), false, true)
  }

  fn from_brand(brand: &AdminBrand) -> Self {
    let mut draft = Self::from_settings(&brand.settings, brand.is_default, false);
    draft.new_id = brand.id.clone();
    draft
  }

  fn from_settings(settings: &BrandSettings, is_default: bool, is_new: bool) -> Self {
    let (title_en, title_de, other_titles) = split_languages(&settings.login_title);
    let (text_en, text_de, other_texts) = split_languages(&settings.login_text);
    let optional = |maybe_text: &Option<String>| maybe_text.clone().unwrap_or_default();
    Self {
      is_new,
      new_id: String::new(),
      is_default,
      display_name: settings.display_name.clone(),
      organization_name: optional(&settings.maybe_organization_name),
      sso_label: optional(&settings.maybe_sso_label),
      hostnames: settings.hostnames.clone(),
      email_domains: list_text(&settings.email_domains),
      title_en,
      title_de,
      text_en,
      text_de,
      other_titles,
      other_texts,
      support: optional(&settings.links.maybe_support),
      privacy: optional(&settings.links.maybe_privacy),
      terms: optional(&settings.links.maybe_terms),
      imprint: optional(&settings.links.maybe_imprint),
      download: optional(&settings.links.maybe_download),
      sender_name: optional(&settings.mail.maybe_sender_name),
      from_address: optional(&settings.mail.maybe_from_address),
      light: colors_of(&settings.colors.light),
      dark: colors_of(&settings.colors.dark),
    }
  }

  fn to_settings(&self) -> BrandSettings {
    let optional = |text: &str| {
      let text = text.trim();
      (!text.is_empty()).then(|| text.to_owned())
    };
    BrandSettings {
      display_name: self.display_name.trim().to_owned(),
      maybe_organization_name: optional(&self.organization_name),
      hostnames: self.hostnames.clone(),
      email_domains: parse_list(&self.email_domains.to_ascii_lowercase()),
      colors: BrandColors {
        light: colors_map(&self.light),
        dark: colors_map(&self.dark),
      },
      login_title: join_languages(&self.title_en, &self.title_de, &self.other_titles),
      login_text: join_languages(&self.text_en, &self.text_de, &self.other_texts),
      links: BrandLinks {
        maybe_support: optional(&self.support),
        maybe_privacy: optional(&self.privacy),
        maybe_terms: optional(&self.terms),
        maybe_imprint: optional(&self.imprint),
        maybe_download: optional(&self.download),
      },
      mail: BrandMailSettings {
        maybe_sender_name: optional(&self.sender_name),
        maybe_from_address: optional(&self.from_address),
      },
      maybe_sso_label: optional(&self.sso_label),
    }
  }

  fn set_hostname(&mut self, hostname: &str, is_on: bool) {
    self.hostnames.retain(|existing| existing != hostname);
    if is_on {
      self.hostnames.push(hostname.to_owned());
    }
  }

  fn set_color(&mut self, is_dark: bool, index: usize, text: String) {
    let colors = if is_dark { &mut self.dark } else { &mut self.light };
    colors[index] = text;
  }

  /// `(key, typed value)` of every color of a theme, in the order of the form.
  fn theme_colors(&self, is_dark: bool) -> Vec<(&'static str, String)> {
    let colors = if is_dark { &self.dark } else { &self.light };
    COLOR_KEYS.into_iter().zip(colors.iter().cloned()).collect()
  }
}

impl Validated for BrandDraft {
  fn errors(&self) -> FieldErrors {
    let mut errors = FieldErrors::new();
    if self.is_new && !is_brand_id(self.new_id.trim()) {
      errors.push(("id", "admin-error-brand-id"));
    }
    let name_length = self.display_name.trim().chars().count();
    if name_length == 0 {
      errors.push(("display_name", "admin-error-required"));
    } else if name_length > MAX_NAME_CHARS {
      errors.push(("display_name", "admin-error-too-long"));
    }
    if self.organization_name.trim().chars().count() > MAX_NAME_CHARS {
      errors.push(("organization_name", "admin-error-too-long"));
    }
    if self.sso_label.trim().chars().count() > MAX_SSO_LABEL_CHARS {
      errors.push(("sso_label", "admin-error-too-long"));
    }
    if !parse_list(&self.email_domains.to_ascii_lowercase())
      .iter()
      .all(|domain| is_email_domain(domain))
    {
      errors.push(("email_domains", "admin-error-domains"));
    }
    if !self.hostnames.iter().all(|hostname| is_dns_name(hostname)) {
      errors.push(("hostnames", "admin-error-host"));
    }
    for (field, text) in [
      ("title_en", &self.title_en),
      ("title_de", &self.title_de),
      ("text_en", &self.text_en),
      ("text_de", &self.text_de),
    ] {
      if text.chars().count() > MAX_TEXT_CHARS {
        errors.push((field, "admin-error-too-long"));
      }
    }
    for (field, link) in [
      ("support", &self.support),
      ("privacy", &self.privacy),
      ("terms", &self.terms),
      ("imprint", &self.imprint),
      ("download", &self.download),
    ] {
      let link = link.trim();
      if !link.is_empty() && !is_web_url(link) {
        errors.push((field, "admin-error-url"));
      }
    }
    if self.sender_name.trim().chars().count() > MAX_NAME_CHARS {
      errors.push(("sender_name", "admin-error-too-long"));
    }
    let from = self.from_address.trim();
    if !from.is_empty() && !is_email_address(from) {
      errors.push(("from_address", "admin-error-email"));
    }
    for is_dark in [false, true] {
      for (index, (_, typed)) in self.theme_colors(is_dark).into_iter().enumerate() {
        let typed = typed.trim();
        if !typed.is_empty() && !is_brand_hex_color(typed) {
          errors.push((COLOR_FIELDS[usize::from(is_dark)][index], "admin-error-color"));
        }
      }
    }
    errors
  }
}

/// `[a-z0-9-]`, 1 to 32 characters.
fn is_brand_id(text: &str) -> bool {
  !text.is_empty()
    && text.len() <= MAX_ID_CHARS
    && text
      .bytes()
      .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn colors_of(colors: &BTreeMap<String, String>) -> [String; 7] {
  COLOR_KEYS.map(|key| colors.get(key).cloned().unwrap_or_default())
}

fn colors_map(colors: &[String; 7]) -> BTreeMap<String, String> {
  COLOR_KEYS
    .iter()
    .zip(colors)
    .filter(|(_, value)| !value.trim().is_empty())
    .map(|(key, value)| ((*key).to_owned(), value.trim().to_owned()))
    .collect()
}

/// English and German texts, and everything else.
fn split_languages(texts: &BTreeMap<String, String>) -> (String, String, BTreeMap<String, String>) {
  let others = texts
    .iter()
    .filter(|(language, _)| language.as_str() != "en" && language.as_str() != "de")
    .map(|(language, text)| (language.clone(), text.clone()))
    .collect();
  (
    texts.get("en").cloned().unwrap_or_default(),
    texts.get("de").cloned().unwrap_or_default(),
    others,
  )
}

fn join_languages(en: &str, de: &str, others: &BTreeMap<String, String>) -> BTreeMap<String, String> {
  let mut texts = others.clone();
  for (language, text) in [("en", en), ("de", de)] {
    let text = text.trim();
    if !text.is_empty() {
      texts.insert(language.to_owned(), text.to_owned());
    }
  }
  texts
}

// ---- Colors (pure, tested) ----------------------------------------------------------------------

/// `#rgb` / `#rrggbb` as bytes.
fn parse_hex(text: &str) -> Option<[u8; 3]> {
  if !is_brand_hex_color(text) {
    return None;
  }
  let digits = &text[1..];
  let byte = |range: std::ops::Range<usize>| u8::from_str_radix(&digits[range], 16).ok();
  if digits.len() == 3 {
    let nibble = |index: usize| byte(index..index + 1).map(|value| value * 17);
    Some([nibble(0)?, nibble(1)?, nibble(2)?])
  } else {
    Some([byte(0..2)?, byte(2..4)?, byte(4..6)?])
  }
}

/// The long form, for the browser's color picker.
fn expand_hex(text: &str) -> Option<String> {
  parse_hex(text.trim()).map(|[r, g, b]| format!("#{r:02x}{g:02x}{b:02x}"))
}

/// WCAG relative luminance.
fn relative_luminance([r, g, b]: [u8; 3]) -> f64 {
  let channel = |value: u8| {
    let value = f64::from(value) / 255.0;
    if value <= 0.03928 {
      value / 12.92
    } else {
      ((value + 0.055) / 1.055).powf(2.4)
    }
  };
  0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

/// WCAG contrast ratio, 1 to 21.
fn contrast_ratio(first: [u8; 3], second: [u8; 3]) -> f64 {
  let (a, b) = (relative_luminance(first), relative_luminance(second));
  let (lighter, darker) = if a >= b { (a, b) } else { (b, a) };
  (lighter + 0.05) / (darker + 0.05)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContrastPair {
  /// The accent against the brand's `on_accent` color.
  OnAccent,
  /// The accent against the better one of the interface's own light and dark text.
  DefaultText,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ContrastCheck {
  pair: ContrastPair,
  ratio: f64,
}

impl ContrastCheck {
  fn is_ok(self) -> bool {
    self.ratio >= MIN_CONTRAST
  }
}

/// The contrast of what is written on the accent color. Nothing to check without a valid accent.
fn contrast_checks(colors: &[(&'static str, String)]) -> Vec<ContrastCheck> {
  let find = |wanted: &str| {
    colors
      .iter()
      .find(|(key, _)| *key == wanted)
      .and_then(|(_, value)| parse_hex(value.trim()))
  };
  let Some(accent) = find("accent") else {
    return Vec::new();
  };
  match find("on_accent") {
    Some(on_accent) => vec![ContrastCheck {
      pair: ContrastPair::OnAccent,
      ratio: contrast_ratio(accent, on_accent),
    }],
    None => vec![ContrastCheck {
      pair: ContrastPair::DefaultText,
      ratio: contrast_ratio(accent, LIGHT_TEXT).max(contrast_ratio(accent, DARK_TEXT)),
    }],
  }
}

/// CSS custom properties for the preview: only valid hex colors reach the element's style.
fn preview_properties(colors: &[(&'static str, String)]) -> Vec<(&'static str, String)> {
  let mut properties = Vec::new();
  for (key, value) in colors {
    let value = value.trim();
    if !is_brand_hex_color(value) {
      continue;
    }
    for property in brand_color_properties(key).unwrap_or_default() {
      properties.push((*property, value.to_owned()));
    }
  }
  properties
}

/// Accepted images: a known type within the size limit.
fn check_asset(content_type: &str, size: u64, max_bytes: u64) -> Result<(), &'static str> {
  if !IMAGE_TYPES.contains(&content_type) {
    Err("admin-error-image-type")
  } else if size == 0 || size > max_bytes {
    Err("admin-error-image-size")
  } else {
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::BRAND_COLOR_TOKENS;

  use super::*;

  fn brand() -> AdminBrand {
    AdminBrand {
      id: "brand-a".into(),
      source: BrandSource::Database,
      is_default: false,
      is_overriding_config: false,
      settings: BrandSettings {
        display_name: "Brand A".into(),
        maybe_organization_name: Some("Brand A Ltd.".into()),
        hostnames: vec!["chat.a.example.com".into()],
        email_domains: vec!["a.example.com".into()],
        colors: BrandColors {
          light: BTreeMap::from([("accent".to_owned(), "#0b7f78".to_owned())]),
          dark: BTreeMap::new(),
        },
        login_title: BTreeMap::from([
          ("en".to_owned(), "Welcome".to_owned()),
          ("fr".to_owned(), "Bienvenue".to_owned()),
        ]),
        login_text: BTreeMap::new(),
        links: BrandLinks {
          maybe_support: Some("https://support.example.com".into()),
          ..BrandLinks::default()
        },
        mail: BrandMailSettings::default(),
        maybe_sso_label: None,
      },
      assets: Vec::new(),
      maybe_updated_at: None,
    }
  }

  #[test]
  fn hex_colors_parse_in_both_forms() {
    assert_eq!(parse_hex("#fff"), Some([255, 255, 255]));
    assert_eq!(parse_hex("#0b7f78"), Some([11, 127, 120]));
    assert_eq!(parse_hex("#abc"), Some([170, 187, 204]));
    for bad in ["", "fff", "#ffff", "#ggg", "red", "#fff;}"] {
      assert_eq!(parse_hex(bad), None, "{bad}");
    }
    assert_eq!(expand_hex(" #abc ").as_deref(), Some("#aabbcc"));
    assert_eq!(expand_hex("nope"), None);
  }

  #[test]
  fn contrast_follows_wcag() {
    let white = [255, 255, 255];
    let black = [0, 0, 0];
    assert!((contrast_ratio(white, black) - 21.0).abs() < 0.01);
    assert!((contrast_ratio(white, white) - 1.0).abs() < 0.001);
    // Known value: #767676 on white is the lightest gray that passes AA (about 4.54).
    let ratio = contrast_ratio(parse_hex("#767676").unwrap(), white);
    assert!((ratio - 4.54).abs() < 0.02, "{ratio}");
    assert_eq!(contrast_ratio(white, black), contrast_ratio(black, white));
  }

  #[test]
  fn low_contrast_is_flagged() {
    let colors = |accent: &str, on_accent: &str| -> Vec<(&'static str, String)> {
      vec![("accent", accent.to_owned()), ("on_accent", on_accent.to_owned())]
    };
    let weak = contrast_checks(&colors("#7fd6cf", "#ffffff"));
    assert_eq!(weak[0].pair, ContrastPair::OnAccent);
    assert!(!weak[0].is_ok());
    assert!(contrast_checks(&colors("#0b4f4a", "#ffffff"))[0].is_ok());
    // Without on_accent the better of the interface's own texts counts: dark text on a light accent.
    let default_text = contrast_checks(&[("accent", "#7fd6cf".to_owned())]);
    assert_eq!(default_text[0].pair, ContrastPair::DefaultText);
    assert!(default_text[0].is_ok());
    // A mid-tone accent fails with both texts.
    assert!(!contrast_checks(&[("accent", "#808080".to_owned())])[0].is_ok());
    assert!(contrast_checks(&[("accent", "oops".to_owned())]).is_empty());
    assert!(contrast_checks(&[]).is_empty());
  }

  #[test]
  fn the_preview_carries_only_valid_colors() {
    let properties = preview_properties(&[
      ("accent", "#0b7f78".to_owned()),
      ("chrome", "red; background:url(x)".to_owned()),
      ("sidebar", "#123".to_owned()),
    ]);
    let expected = [
      ("--accent", "#0b7f78"),
      ("--focus", "#0b7f78"),
      ("--sb-bg", "#123"),
    ];
    assert_eq!(
      properties,
      expected.map(|(property, value)| (property, value.to_owned()))
    );
  }

  #[test]
  fn a_brand_round_trips_through_the_draft() {
    let original = brand();
    let draft = BrandDraft::from_brand(&original);
    assert!(draft.errors().is_empty());
    assert_eq!(draft.title_en, "Welcome");
    assert_eq!(draft.to_settings(), original.settings);
  }

  #[test]
  fn empty_optional_parts_are_left_out_and_texts_are_trimmed() {
    let mut draft = BrandDraft::from_brand(&brand());
    draft.organization_name = "  ".into();
    draft.support.clear();
    draft.title_de = " Willkommen ".into();
    draft.set_color(true, 0, " #35c6ba ".into());
    draft.email_domains = "A.example.com, b.example.com\n".into();
    let settings = draft.to_settings();
    assert_eq!(settings.maybe_organization_name, None);
    assert_eq!(settings.links.maybe_support, None);
    assert_eq!(settings.login_title["de"], "Willkommen");
    assert_eq!(settings.login_title["fr"], "Bienvenue");
    assert_eq!(settings.colors.dark["accent"], "#35c6ba");
    assert_eq!(settings.email_domains, ["a.example.com", "b.example.com"]);
  }

  #[test]
  fn obvious_mistakes_are_reported_per_field() {
    let mut draft = BrandDraft::new_brand();
    let fields = |draft: &BrandDraft| -> Vec<&'static str> {
      draft.errors().into_iter().map(|(field, _)| field).collect()
    };
    assert_eq!(fields(&draft), ["id", "display_name"]);
    draft.new_id = "Brand A".into();
    draft.display_name = "Brand A".into();
    draft.privacy = "http://example.com/privacy".into();
    draft.from_address = "chat".into();
    draft.email_domains = "localhost".into();
    draft.set_color(false, 2, "white".into());
    draft.set_color(true, 0, "#12".into());
    assert_eq!(
      fields(&draft),
      [
        "id",
        "email_domains",
        "privacy",
        "from_address",
        "color_light_on_accent",
        "color_dark_accent"
      ]
    );
    draft.new_id = "brand-a".into();
    assert!(!fields(&draft).contains(&"id"));
  }

  #[test]
  fn brand_ids_are_lower_case_slugs() {
    for ok in ["a", "brand-a", "b2"] {
      assert!(is_brand_id(ok), "{ok}");
    }
    for bad in ["", "Brand", "a b", "a_b", &"x".repeat(33)] {
      assert!(!is_brand_id(bad), "{bad}");
    }
  }

  #[test]
  fn host_names_are_toggled_and_their_use_is_told() {
    let mut draft = BrandDraft::from_brand(&brand());
    draft.set_hostname("team.a.example.com", true);
    draft.set_hostname("chat.a.example.com", false);
    assert_eq!(draft.hostnames, ["team.a.example.com"]);
    let served = |brand_id: Option<&str>| ServedHostname {
      hostname: "x.example.com".into(),
      maybe_brand_id: brand_id.map(str::to_owned),
    };
    assert_eq!(
      hostname_state(&served(Some("brand-a")), "brand-a"),
      HostState::Active
    );
    assert_eq!(
      hostname_state(&served(Some("brand-b")), "brand-a"),
      HostState::UsedBy("brand-b".into())
    );
    assert_eq!(hostname_state(&served(None), "brand-a"), HostState::Unused);
  }

  #[test]
  fn images_are_checked_before_upload() {
    assert_eq!(check_asset("image/png", 1_000, 2_000), Ok(()));
    assert_eq!(
      check_asset("image/gif", 1_000, 2_000),
      Err("admin-error-image-type")
    );
    assert_eq!(
      check_asset("image/png", 3_000, 2_000),
      Err("admin-error-image-size")
    );
    assert_eq!(check_asset("image/png", 0, 2_000), Err("admin-error-image-size"));
  }

  #[test]
  fn color_keys_match_the_wire_tokens() {
    let wire: Vec<&str> = BRAND_COLOR_TOKENS.iter().map(|(key, _)| *key).collect();
    assert_eq!(wire, COLOR_KEYS);
  }

  #[test]
  fn every_label_is_translated() {
    use crate::i18n::{Locale, translate};

    let mut keys: Vec<String> = [
      "admin-brands-title",
      "admin-brands-subtitle",
      "admin-brands-list",
      "admin-brands-new",
      "admin-brands-name",
      "admin-brands-default",
      "admin-brands-overrides",
      "admin-brands-view",
      "admin-brands-edit",
      "admin-brands-source-builtin",
      "admin-brands-source-config",
      "admin-brands-source-database",
      "admin-brands-builtin-note",
      "admin-brands-config-note",
      "admin-brands-id",
      "admin-brands-id-hint",
      "admin-brands-display-name",
      "admin-brands-organization",
      "admin-brands-organization-hint",
      "admin-brands-sso-label",
      "admin-brands-sso-label-hint",
      "admin-brands-is-default",
      "admin-brands-is-default-hint",
      "admin-brands-hosts",
      "admin-brands-hosts-hint",
      "admin-brands-host-active",
      "admin-brands-host-used-by",
      "admin-brands-host-unused",
      "admin-brands-host-not-served",
      "admin-brands-email-domains",
      "admin-brands-email-domains-hint",
      "admin-brands-login",
      "admin-brands-title-en",
      "admin-brands-title-de",
      "admin-brands-text-en",
      "admin-brands-text-de",
      "admin-brands-links",
      "admin-brands-link-support",
      "admin-brands-link-privacy",
      "admin-brands-link-terms",
      "admin-brands-link-imprint",
      "admin-brands-link-download",
      "admin-brands-mail",
      "admin-brands-mail-hint",
      "admin-brands-sender-name",
      "admin-brands-from-address",
      "admin-brands-colors",
      "admin-brands-colors-hint",
      "admin-brands-colors-light",
      "admin-brands-colors-dark",
      "admin-brands-color-picker",
      "admin-brands-preview-button",
      "admin-brands-preview-soft",
      "admin-brands-contrast-on-accent",
      "admin-brands-contrast-default-text",
      "admin-brands-contrast-warning",
      "admin-brands-images",
      "admin-brands-images-hint",
      "admin-brands-images-unavailable",
      "admin-brands-image-set",
      "admin-brands-image-none",
      "admin-brands-image-delete",
      "admin-brands-take-over",
      "admin-brands-take-over-hint",
      "admin-brands-delete",
      "admin-brands-delete-restore",
      "admin-brands-delete-confirm",
      "admin-brands-close",
      "admin-settings-loading",
      "admin-save",
      "admin-error-brand-id",
      "admin-error-required",
      "admin-error-too-long",
      "admin-error-domains",
      "admin-error-host",
      "admin-error-url",
      "admin-error-email",
      "admin-error-color",
      "admin-error-image-type",
      "admin-error-image-size",
      "admin-error-image-unreadable",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    keys.extend(SLOTS.iter().map(|(key, _)| (*key).to_owned()));
    for key in COLOR_KEYS {
      keys.push(format!("admin-brands-color-{}", key.replace('_', "-")));
    }
    for locale in [Locale::En, Locale::De] {
      for key in &keys {
        assert_ne!(&translate(locale, key, None), key, "{locale:?} lacks {key}");
      }
    }
  }
}
