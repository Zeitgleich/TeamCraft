use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_proto::{BotResponse, BotTokenResponse};
use wasm_bindgen::JsCast;

use crate::clock::format_date_time;
use crate::components::admin_console::SectionHeader;
use crate::components::copy_button::CopyButton;
use crate::components::icons::{Icon, IconName};
use crate::components::image_crop_dialog::{CropTarget, CropUpload, ImageCropDialog};
use crate::feedback::report_failure;
use crate::i18n::use_i18n;
use crate::image_crop::check_pick;
use crate::integrations::{BotsHub, MAX_BOT_NAME_CHARS, bridge_config, use_bots};
use crate::reactive::SetIfAlive;
use crate::rest::RestError;

/// Bot accounts for the integration bridge (docs/bridge.md): create one, copy its token and the
/// bridge configuration (shown once), rotate or revoke. Adding a bot to channels is a channel
/// setting.
#[component]
pub fn IntegrationsSection() -> impl IntoView {
  let i18n = use_i18n();
  let Some(hub) = use_bots() else {
    return view! { <p class="muted-text">{move || i18n.t("admin-integrations-unavailable")}</p> }.into_any();
  };
  hub.refresh();
  let name = RwSignal::new(String::new());
  let is_busy = RwSignal::new(false);
  let create = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    let bot_name = name.get_untracked().trim().to_owned();
    if bot_name.is_empty() || is_busy.get_untracked() {
      return;
    }
    is_busy.set(true);
    spawn_local(async move {
      match hub.create(bot_name).await {
        Ok(()) => name.set_if_alive(String::new()),
        Err(error) => report_failure(error.to_failure()),
      }
      is_busy.set_if_alive(false);
    });
  };

  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-integrations-title"))
      subtitle=Signal::derive(move || i18n.t("admin-integrations-subtitle"))
    />
    {move || hub.maybe_reveal.get().map(|reveal| view! { <TokenReveal hub=hub reveal=reveal /> })}
    <form class="integration-create" on:submit=create>
      <label for="bot-name">{move || i18n.t("admin-integrations-name")}</label>
      <input
        id="bot-name"
        class="input"
        required
        maxlength=MAX_BOT_NAME_CHARS.to_string()
        placeholder=move || i18n.t("admin-integrations-name-example")
        prop:value=move || name.get()
        on:input=move |ev| name.set(event_target_value(&ev))
      />
      <button type="submit" class="btn btn-primary" prop:disabled=move || is_busy.get() || name.get().trim().is_empty()>
        <Icon name=IconName::Plus />
        {move || i18n.t("admin-integrations-create")}
      </button>
    </form>
    <p class="muted-text small">{move || i18n.t("admin-integrations-hint")}</p>
    <div class="table-wrap">
      <table class="data-table">
        <caption class="visually-hidden">{move || i18n.t("admin-integrations-title")}</caption>
        <thead>
          <tr>
            <th scope="col">{move || i18n.t("admin-integrations-col-bot")}</th>
            <th scope="col">{move || i18n.t("admin-integrations-col-created")}</th>
            <th scope="col">{move || i18n.t("admin-integrations-col-status")}</th>
            <th scope="col"><span class="visually-hidden">{move || i18n.t("retention-actions")}</span></th>
          </tr>
        </thead>
        <tbody>
          <For each=move || hub.bots.get() key=|bot| (bot.id, bot.is_revoked) let:bot>
            <BotRow hub=hub bot=bot />
          </For>
        </tbody>
      </table>
    </div>
    <Show when=move || hub.is_loaded.get() && hub.bots.with(Vec::is_empty) fallback=|| ()>
      <p class="empty-state">{move || i18n.t("admin-integrations-empty")}</p>
    </Show>
  }
  .into_any()
}

#[component]
fn BotRow(hub: BotsHub, bot: BotResponse) -> impl IntoView {
  let i18n = use_i18n();
  let id = bot.id;
  let is_revoked = bot.is_revoked;
  let is_paused = bot.is_paused;
  let is_confirming = RwSignal::new(false);
  let is_busy = RwSignal::new(false);
  let run = move |work: std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), RestError>>>>| {
    if is_busy.get_untracked() {
      return;
    }
    is_busy.set(true);
    spawn_local(async move {
      if let Err(error) = work.await {
        report_failure(error.to_failure());
      }
      is_busy.set_if_alive(false);
      is_confirming.set_if_alive(false);
    });
  };
  let picked = RwSignal::new_local(None::<web_sys::File>);
  let pick_error = RwSignal::new(None::<&'static str>);
  let pick = move |ev: leptos::ev::Event| {
    let Some(input) = ev
      .target()
      .and_then(|target| target.dyn_into::<web_sys::HtmlInputElement>().ok())
    else {
      return;
    };
    let maybe_file = input.files().and_then(|files| files.item(0));
    input.set_value("");
    let Some(file) = maybe_file else {
      return;
    };
    match check_pick(&file.type_(), file.size()) {
      Ok(()) => {
        pick_error.set(None);
        picked.set(Some(file));
      },
      Err(error) => pick_error.set(Some(error.i18n_key())),
    }
  };
  let apply_picture = Callback::new(move |png: Vec<u8>| -> CropUpload {
    Box::pin(async move {
      hub
        .set_picture(id, png)
        .await
        .map_err(|error| error.to_failure())?;
      picked.set_if_alive(None);
      Ok(())
    })
  });
  view! {
    <tr>
      <th scope="row" translate="no">{bot.name.clone()}</th>
      <td>{format_date_time(bot.created_at.0, i18n.locale().tag())}</td>
      <td>
        <span class="badge" class:badge-guest=is_revoked>
          {move || i18n.t(if is_revoked {
            "admin-integrations-revoked"
          } else if is_paused {
            "admin-integrations-paused"
          } else {
            "admin-integrations-active"
          })}
        </span>
        {is_paused.then(|| view! { <p class="muted-text small">{move || i18n.t("admin-integrations-paused-hint")}</p> })}
      </td>
      <td class="row-actions">
        {(!is_revoked)
          .then(|| {
            view! {
              <label class="btn btn-small">
                <Icon name=IconName::Camera />
                <span>{move || i18n.t("admin-integrations-picture")}</span>
                <input type="file" class="visually-hidden" accept="image/png,image/jpeg,image/webp" on:change=pick />
              </label>
              <button type="button" class="btn btn-small btn-quiet" prop:disabled=move || is_busy.get() on:click=move |_| run(Box::pin(async move { hub.remove_picture(id).await }))>
                {move || i18n.t("admin-integrations-picture-remove")}
              </button>
              <button type="button" class="btn btn-small" prop:disabled=move || is_busy.get() on:click=move |_| run(Box::pin(async move { hub.rotate(id).await }))>
                {move || i18n.t("admin-integrations-rotate")}
              </button>
              <Show
                when=move || is_confirming.get()
                fallback=move || {
                  view! {
                    <button type="button" class="btn btn-small btn-quiet" on:click=move |_| is_confirming.set(true)>
                      {move || i18n.t("admin-integrations-revoke")}
                    </button>
                  }
                }
              >
                <button type="button" class="btn btn-small btn-danger" prop:disabled=move || is_busy.get() on:click=move |_| run(Box::pin(async move { hub.revoke(id).await }))>
                  {move || i18n.t("admin-integrations-revoke-confirm")}
                </button>
                <button type="button" class="btn btn-small" on:click=move |_| is_confirming.set(false)>
                  {move || i18n.t("cancel")}
                </button>
              </Show>
            }
          })}
        {move || pick_error.get().map(|key| view! { <p class="field-error" role="alert">{move || i18n.t(key)}</p> })}
        {move || picked.get().map(|file| view! {
          <ImageCropDialog
            file=file
            target=CropTarget::IntegrationPicture
            on_apply=apply_picture
            on_cancel=Callback::new(move |()| picked.set(None))
          />
        })}
      </td>
    </tr>
  }
}

/// The token of a bot just created or re-keyed, and the bridge configuration with it. The server
/// keeps only a hash: closing this loses the token for good.
#[component]
fn TokenReveal(hub: BotsHub, reveal: BotTokenResponse) -> impl IntoView {
  let i18n = use_i18n();
  let server = crate::components::space_invites::web_origin().unwrap_or_default();
  let config = bridge_config(&server, &reveal.bot.name);
  let token = reveal.token.clone();
  let bot_name = reveal.bot.name.clone();
  view! {
    <section class="integration-reveal" role="status" aria-live="polite">
      <h3>{move || i18n.t_with("admin-integrations-reveal-title", &[("name", &bot_name)])}</h3>
      <p class="muted-text">{move || i18n.t("admin-integrations-reveal-once")}</p>
      <label for="bot-token">{move || i18n.t("admin-integrations-token")}</label>
      <div class="integration-copy-row">
        <input id="bot-token" class="input mono" readonly prop:value=token.clone() on:focus=|ev| select_all(&ev) />
        <CopyButton text=token.clone() />
      </div>
      <label for="bot-config">{move || i18n.t("admin-integrations-config")}</label>
      <textarea id="bot-config" class="input mono" rows="7" readonly prop:value=config.clone() on:focus=|ev| select_all(&ev)></textarea>
      <div class="integration-copy-row">
        <CopyButton text=config.clone() />
        <span class="grow"></span>
        <button type="button" class="btn" on:click=move |_| hub.maybe_reveal.set(None)>
          {move || i18n.t("admin-integrations-reveal-done")}
        </button>
      </div>
      <p class="muted-text small">{move || i18n.t("admin-integrations-next-steps")}</p>
    </section>
  }
}

fn select_all(ev: &leptos::ev::FocusEvent) {
  use wasm_bindgen::JsCast;
  if let Some(target) = ev.target() {
    if let Some(input) = target.dyn_ref::<web_sys::HtmlInputElement>() {
      input.select();
    } else if let Some(area) = target.dyn_ref::<web_sys::HtmlTextAreaElement>() {
      area.select();
    }
  }
}
