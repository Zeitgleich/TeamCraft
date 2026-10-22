use leptos::html;
use leptos::prelude::*;
use teamcraft_client_api::calls::OrgBackgroundRule;
use teamcraft_proto::{CallBackgroundMode, OrgBackgroundId};
use wasm_bindgen_futures::JsFuture;

use crate::admin::PolicyBlocker;
use crate::components::admin_console::SectionHeader;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::state::use_app_state;

const MODES: [CallBackgroundMode; 3] = [
  CallBackgroundMode::Free,
  CallBackgroundMode::OrgOnly,
  CallBackgroundMode::Required,
];

/// Organization call backgrounds (admin.md §3.5): owners upload pictures, order them, pick the
/// default and decide whether people must use them. Every change is a new signed policy version
/// that member clients verify and enforce.
#[component]
pub fn BackgroundsSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let current = Memo::new(move |_| state.read(|backend| backend.org_backgrounds()));
  let blocker = Memo::new(move |_| state.read(|backend| backend.policy_edit_blocker()));
  let is_locked = move || blocker.get().is_some();
  let mode = RwSignal::new(current.get_untracked().mode);
  let guests_exempt = RwSignal::new(current.get_untracked().guests_exempt);
  Effect::new(move |_| {
    let latest = current.get();
    mode.set(latest.mode);
    guests_exempt.set(latest.guests_exempt);
  });

  // Every change is the whole rule; the order is the current one unless given.
  let publish = move |maybe_order: Option<Vec<OrgBackgroundId>>, maybe_default: Option<OrgBackgroundId>| {
    let latest = current.get_untracked();
    let rule = OrgBackgroundRule {
      mode: mode.get_untracked(),
      maybe_default,
      guests_exempt: guests_exempt.get_untracked(),
      order: maybe_order.unwrap_or_else(|| latest.images.iter().map(|image| image.id).collect()),
    };
    state.write(|backend| backend.set_org_background_rule(rule));
  };
  let move_image = move |index: usize, is_up: bool| {
    let latest = current.get_untracked();
    let mut order: Vec<OrgBackgroundId> = latest.images.iter().map(|image| image.id).collect();
    let target = if is_up {
      index.checked_sub(1)
    } else {
      Some(index + 1)
    };
    if let Some(target) = target.filter(|target| *target < order.len()) {
      order.swap(index, target);
      publish(Some(order), latest.maybe_default);
    }
  };

  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-backgrounds-title"))
      subtitle=Signal::derive(move || i18n.t("admin-backgrounds-subtitle"))
    />
    {move || {
      blocker.get().map(|blocker| {
        let key = match blocker {
          PolicyBlocker::NotOwner => "policy-blocked-not-owner",
          PolicyBlocker::NoSigningKeys => "policy-blocked-no-keys",
        };
        view! { <p class="auth-alert policy-blocked" role="status">{move || i18n.t(key)}</p> }
      })
    }}
    <form
      class="policy-form"
      on:submit=move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        publish(None, current.get_untracked().maybe_default);
      }
    >
      <fieldset class="policy-group" prop:disabled=is_locked>
        <legend>{move || i18n.t("policy-backgrounds-mode")}</legend>
        {MODES
          .into_iter()
          .map(|option| {
            let slug = mode_slug(&option);
            let checked_option = option.clone();
            let chosen_option = option.clone();
            view! {
              <label class="radio">
                <input
                  type="radio"
                  name="call_background_mode"
                  prop:checked=move || mode.get() == checked_option
                  on:change=move |_| mode.set(chosen_option.clone())
                />
                <span>
                  {move || i18n.t(&format!("policy-backgrounds-{slug}"))}
                  <small class="muted-text">{move || i18n.t(&format!("policy-backgrounds-{slug}-hint"))}</small>
                </span>
              </label>
            }
          })
          .collect_view()}
        <label class="checkbox">
          <input
            type="checkbox"
            prop:checked=move || guests_exempt.get()
            on:change=move |ev| guests_exempt.set(event_target_checked(&ev))
          />
          <span>{move || i18n.t("policy-backgrounds-guests-exempt")}</span>
        </label>
        <p class="muted-text small" role="status">
          {move || {
            (mode.get() == CallBackgroundMode::Required && current.get().images.is_empty())
              .then(|| i18n.t("policy-backgrounds-required-needs-picture"))
          }}
        </p>
        <button
          class="btn btn-primary"
          type="submit"
          prop:disabled=move || mode.get() == CallBackgroundMode::Required && current.get().images.is_empty()
        >
          {move || i18n.t("policy-save")}
        </button>
      </fieldset>
    </form>
    <section class="policy-group" aria-label=move || i18n.t("admin-backgrounds-pictures")>
      <h3>{move || i18n.t("admin-backgrounds-pictures")}</h3>
      <ul class="admin-list">
        {move || {
          let latest = current.get();
          let count = latest.images.len();
          latest
            .images
            .into_iter()
            .enumerate()
            .map(|(index, image)| {
              let id = image.id;
              let is_default = latest.maybe_default == Some(id);
              view! {
                <li class="admin-row">
                  <Icon name=IconName::Image />
                  <span class="admin-row-name">{image.name}</span>
                  <label class="radio">
                    <input
                      type="radio"
                      name="default_background"
                      prop:checked=is_default
                      prop:disabled=is_locked
                      on:change=move |_| publish(None, Some(id))
                    />
                    <span>{move || i18n.t("admin-backgrounds-default")}</span>
                  </label>
                  <button
                    class="icon-btn"
                    title=move || i18n.t("admin-backgrounds-up")
                    prop:disabled=move || is_locked() || index == 0
                    on:click=move |_| move_image(index, true)
                  >
                    <Icon name=IconName::ChevronRight class="rotate-up" />
                  </button>
                  <button
                    class="icon-btn"
                    title=move || i18n.t("admin-backgrounds-down")
                    prop:disabled=move || is_locked() || index + 1 == count
                    on:click=move |_| move_image(index, false)
                  >
                    <Icon name=IconName::ChevronRight class="rotate-down" />
                  </button>
                  <button
                    class="btn btn-danger btn-small"
                    prop:disabled=is_locked
                    on:click=move |_| state.write(|backend| backend.delete_org_background(id))
                  >
                    {move || i18n.t("admin-backgrounds-delete")}
                  </button>
                </li>
              }
            })
            .collect_view()
        }}
      </ul>
      <UploadForm is_locked=Signal::derive(is_locked) />
    </section>
  }
}

/// Adds a picture: a name and a JPEG or PNG file (the host re-encodes it before upload).
#[component]
fn UploadForm(is_locked: Signal<bool>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let name = RwSignal::new(String::new());
  let file_input = NodeRef::<html::Input>::new();
  let maybe_error = RwSignal::new(None::<String>);
  let upload = move |ev: web_sys::SubmitEvent| {
    ev.prevent_default();
    let Some(file) = file_input
      .get_untracked()
      .and_then(|element| element.files())
      .and_then(|files| files.item(0))
    else {
      maybe_error.set(Some(i18n.t("admin-backgrounds-choose-file")));
      return;
    };
    let chosen_name = match name.get_untracked().trim() {
      "" => file.name(),
      typed => typed.to_owned(),
    };
    maybe_error.set(None);
    leptos::task::spawn_local(async move {
      match JsFuture::from(file.array_buffer()).await {
        Ok(buffer) => {
          let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
          state.write(|backend| backend.add_org_background(&chosen_name, bytes));
          name.set(String::new());
          if let Some(element) = file_input.get_untracked() {
            element.set_value("");
          }
        },
        Err(_) => maybe_error.set(Some(i18n.t("admin-backgrounds-unreadable"))),
      }
    });
  };
  view! {
    <form class="admin-upload" on:submit=upload>
      <fieldset prop:disabled=move || is_locked.get()>
        <input
          class="input"
          type="text"
          maxlength="100"
          placeholder=move || i18n.t("admin-backgrounds-name")
          prop:value=move || name.get()
          on:input=move |ev| name.set(event_target_value(&ev))
        />
        <input type="file" accept="image/jpeg,image/png" node_ref=file_input />
        <button class="btn" type="submit">{move || i18n.t("admin-backgrounds-add")}</button>
      </fieldset>
      {move || maybe_error.get().map(|error| view! { <p class="auth-alert" role="alert">{error}</p> })}
    </form>
  }
}

fn mode_slug(mode: &CallBackgroundMode) -> &'static str {
  match mode {
    CallBackgroundMode::OrgOnly => "org-only",
    CallBackgroundMode::Required => "required",
    _ => "free",
  }
}
