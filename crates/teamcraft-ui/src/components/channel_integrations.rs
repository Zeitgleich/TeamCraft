use std::collections::HashSet;
use std::sync::Arc;

use leptos::prelude::*;
use teamcraft_client_api::actions::ChannelChanges;
use teamcraft_client_api::model::{IntegrationSource, NewIntegrationSource};
use teamcraft_crypto::source_token::SourceToken;
use teamcraft_proto::{
  HistoryPolicy, MAX_SOURCE_NAME_CHARS, Permission, SourceId, SourceKind, SourceOptions,
};
use wasm_bindgen::JsCast;

use crate::channel_access::ChannelAccess;
use crate::components::admin_integrations::CopyButton;
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::icons::{Icon, IconName};
use crate::components::image_crop_dialog::{CropTarget, CropUpload, ImageCropDialog};
use crate::components::pending_button::PendingButton;
use crate::components::roster_banner::unsigned_reason;
use crate::components::space_invites::web_origin;
use crate::components::workspace_dialogs::FailureNote;
use crate::feedback::{ActionFailure, FailureKind, PendingAction};
use crate::form_errors::{FieldError, FieldErrors, FormErrors};
use crate::i18n::use_i18n;
use crate::image_crop::{check_pick, fit_source_picture};
use crate::integrations::{
  BridgeLocation, NewSource, bridge_base_url, ingest_url, is_valid_source_name, webhook_url,
};
use crate::model::{ConversationId, ConversationKind, User, UserId};
use crate::reactive::SetIfAlive;
use crate::state::use_app_state;

/// How many bots the search lists at most.
const SOURCE_FIELD_ID: &str = "channel-source-field";
const SOURCE_NAME_ID: &str = "channel-source-name";
const MAX_BOT_RESULTS: usize = 8;
/// What the crop dialog hands back for a source's picture (`CropTarget::IntegrationPicture`).
const CROPPED_SOURCE_PICTURE_PX: u32 = 256;

/// The "Integrations" section of the channel settings (docs/bridge.md): normal or log mode, the
/// bots in the channel with adding and removing, and the channel's sources. A bot is a normal
/// member (a guest of the space), so any channel can have one; adding it commits its device into
/// the channel's encryption group like adding a person.
#[component]
pub fn ChannelIntegrations(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let access = ChannelAccess::new(state, conversation);
  let can_manage = move || access.can(&Permission::ManageChannelProperties);
  let info = Memo::new(move |_| state.read(|backend| backend.conversation(conversation)));
  let kind = Memo::new(move |_| info.get().map(|info| info.kind));
  let is_log = move || kind.get() == Some(ConversationKind::Integration);
  let is_public = move || kind.get() == Some(ConversationKind::Public);
  // Sources are announced in the channel; a bot added later can't read the announcement.
  let has_bot = Memo::new(move |_| {
    let ids = info.get().map(|info| info.member_ids).unwrap_or_default();
    state.read(|backend| {
      ids
        .iter()
        .filter_map(|id| backend.user(*id))
        .any(|user| user.is_bot)
    })
  });
  let mode_pending = PendingAction::new();
  let set_mode = move |is_integration: bool| {
    if is_log() == is_integration {
      return;
    }
    mode_pending.run(|| {
      state.write(|backend| {
        backend.update_channel(
          conversation,
          ChannelChanges {
            maybe_is_integration: Some(is_integration),
            ..ChannelChanges::default()
          },
        )
      })
    });
  };

  view! {
    <div class="channel-integrations">
    <h3 class="detail-heading">{move || i18n.t("channel-mode")}</h3>
    <div class="segmented" role="radiogroup" aria-label=move || i18n.t("channel-mode")>
      <button
        type="button"
        role="radio"
        aria-checked=move || (!is_log()).to_string()
        aria-pressed=move || (!is_log()).to_string()
        prop:disabled=move || !can_manage() || mode_pending.is_pending()
        on:click=move |_| set_mode(false)
      >
        {move || i18n.t("channel-mode-normal")}
      </button>
      <button
        type="button"
        role="radio"
        aria-checked=move || is_log().to_string()
        aria-pressed=move || is_log().to_string()
        prop:disabled=move || !can_manage() || is_public() || mode_pending.is_pending()
        on:click=move |_| set_mode(true)
      >
        {move || i18n.t("channel-mode-log")}
      </button>
    </div>
    <p class="hint">
      {move || {
        if is_public() {
          i18n.t("channel-mode-public-hint")
        } else if is_log() {
          i18n.t("channel-mode-log-hint")
        } else {
          i18n.t("channel-mode-normal-hint")
        }
      }}
    </p>
    <FailureNote pending=mode_pending />
    <BotMembers conversation=conversation can_manage=Signal::derive(can_manage) />
    <Sources conversation=conversation has_bot=Signal::from(has_bot) />
    </div>
  }
}

/// The bots in the channel and a search for more.
#[component]
fn BotMembers(conversation: ConversationId, can_manage: Signal<bool>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let query = RwSignal::new(String::new());
  let member_ids = Memo::new(move |_| {
    state
      .read(|backend| backend.conversation(conversation))
      .map(|info| info.member_ids.into_iter().collect::<HashSet<_>>())
      .unwrap_or_default()
  });
  let bots_in = Memo::new(move |_| {
    let ids = member_ids.get();
    let mut bots: Vec<User> = state.read(|backend| {
      ids
        .iter()
        .filter_map(|id| backend.user(*id))
        .filter(|user| user.is_bot)
        .collect()
    });
    bots.sort_by_key(|bot| bot.display_name.to_lowercase());
    bots
  });
  let candidates = Memo::new(move |_| {
    let ids = member_ids.get();
    let text = query.get();
    state.read(|backend| {
      backend
        .search_people(text.trim())
        .into_iter()
        .filter(|user| user.is_bot && !ids.contains(&user.id))
        .take(MAX_BOT_RESULTS)
        .collect::<Vec<_>>()
    })
  });
  // The space the channel is in decides whether the bot may be put in it first (as a guest).
  let joins_space = state
    .read(|backend| backend.conversation(conversation))
    .and_then(|info| info.maybe_space)
    .and_then(|id| {
      state
        .read(|backend| backend.spaces())
        .into_iter()
        .find(|space| space.id == id)
    })
    .is_some_and(|space| space.can_add_members || space.can_invite_guests);

  view! {
    <h3 class="detail-heading">{move || i18n.t("channel-bots")}</h3>
    <ul class="settings-member-list">
      <For each=move || bots_in.get() key=|bot| bot.id let:bot>
        <BotMemberRow conversation=conversation bot=bot can_manage=can_manage />
      </For>
    </ul>
    <Show when=move || bots_in.with(Vec::is_empty) fallback=|| ()>
      <p class="muted-text">{move || i18n.t("channel-bots-none")}</p>
    </Show>
    <Show when=move || can_manage.get() fallback=|| ()>
      <div class="field">
        <label for="channel-bot-search">{move || i18n.t("channel-bots-add")}</label>
        <input
          id="channel-bot-search"
          class="input"
          type="search"
          placeholder=move || i18n.t("channel-bots-search")
          prop:value=move || query.get()
          on:input=move |ev| query.set(event_target_value(&ev))
        />
      </div>
      <ul class="settings-member-list">
        <For each=move || candidates.get() key=|bot| bot.id let:bot>
          <BotCandidateRow conversation=conversation bot=bot joins_space=joins_space />
        </For>
      </ul>
      <p class="hint">{move || i18n.t("channel-bots-hint")}</p>
    </Show>
  }
}

#[component]
fn BotMemberRow(conversation: ConversationId, bot: User, can_manage: Signal<bool>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = bot.id;
  let is_confirming = RwSignal::new(false);
  let pending = PendingAction::new();
  view! {
    <li class="settings-member">
      <span class="grow" translate="no">{bot.display_name.clone()}</span>
      <span class="badge">{move || i18n.t("badge-bot")}</span>
      <Show when=move || can_manage.get() fallback=|| ()>
        <Show
          when=move || is_confirming.get()
          fallback=move || {
            view! {
              <button type="button" class="btn btn-small btn-quiet" on:click=move |_| is_confirming.set(true)>
                {move || i18n.t("channel-bots-remove")}
              </button>
            }
          }
        >
          <button
            type="button"
            class="btn btn-small btn-danger"
            prop:disabled=move || pending.is_pending()
            on:click=move |_| pending.run(|| state.write(|backend| backend.remove_member(conversation, id)))
          >
            {move || i18n.t("channel-bots-remove-confirm")}
          </button>
          <button type="button" class="btn btn-small" on:click=move |_| is_confirming.set(false)>
            {move || i18n.t("cancel")}
          </button>
        </Show>
      </Show>
    </li>
  }
}

#[component]
fn BotCandidateRow(conversation: ConversationId, bot: User, joins_space: bool) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = bot.id;
  let pending = PendingAction::new();
  view! {
    <li class="settings-member">
      <span class="grow" translate="no">{bot.display_name.clone()}</span>
      <button
        type="button"
        class="btn btn-small btn-primary"
        prop:disabled=move || pending.is_pending()
        aria-busy=move || pending.is_pending().then_some("true")
        on:click=move |_| {
          // A bot never reads what was written before it was added.
          pending.run(|| {
            state.write(|backend| {
              backend.add_people_to_channel(conversation, vec![(id, true)], HistoryPolicy::FromJoin, joins_space)
            })
          })
        }
      >
        <Icon name=IconName::Plus />
        {move || i18n.t("channel-bots-add-button")}
      </button>
    </li>
  }
}

/// The channel's bridge sources (docs/bridge.md, "Sources"): the ones in force with revoke, and
/// adding one. Adding generates the token here, announces only its hash in the channel (an
/// encrypted control message the bridge reads) and shows the URL once; nothing goes into the
/// bridge's configuration. Only admins of the verified workspace roster manage sources (the bridge
/// doesn't read the signed channel roles yet, e2ee.md §13.1); a creator may revoke their
/// own.
#[component]
fn Sources(conversation: ConversationId, has_bot: Signal<bool>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let sources = Memo::new(move |_| state.read(|backend| backend.integration_sources(conversation)));
  let can_manage = Signal::derive(move || state.read(|backend| backend.can_manage_integration_sources()));
  // The bot that posts for the sources: its picture stands in for a source without one, as in the
  // messages. With several bots in the channel it isn't known which one serves them.
  // A memo of the ID, like `has_bot`: the rows' avatars are built again only when it changes, not
  // on every write to the backend (issue #687).
  let maybe_bot = Memo::new(move |_| {
    let ids = state
      .read(|backend| backend.conversation(conversation))
      .map(|info| info.member_ids)
      .unwrap_or_default();
    let bots: Vec<UserId> = state.read(|backend| {
      ids
        .iter()
        .filter_map(|id| backend.user(*id))
        .filter(|user| user.is_bot)
        .map(|user| user.id)
        .collect()
    });
    stand_in_bot(&bots)
  });
  let maybe_reveal: RwSignal<Option<Revealed>> = RwSignal::new(None);
  view! {
    <h3 class="detail-heading">{move || i18n.t("channel-sources")}</h3>
    <ul class="settings-member-list">
      <For each=move || sources.get() key=|source| (source.id, source.name.clone(), source.maybe_picture_sha256) let:source>
        <SourceRow conversation=conversation source=source can_manage=can_manage maybe_bot=maybe_bot />
      </For>
    </ul>
    <Show when=move || sources.with(Vec::is_empty) fallback=|| ()>
      <p class="muted-text">{move || i18n.t("channel-sources-none")}</p>
    </Show>
    <Show
      when=move || maybe_reveal.with(Option::is_some)
      fallback=move || {
        view! {
          <Show
            when=move || can_manage.get()
            fallback=move || view! { <p class="hint">{move || who_adds_sources(i18n, state)}</p> }
          >
            <AddSource conversation=conversation has_bot=has_bot on_added=move |revealed| maybe_reveal.set(Some(revealed)) />
          </Show>
        }
      }
    >
      {move || maybe_reveal.get().map(|revealed| view! { <RevealSource revealed=revealed on_done=move || maybe_reveal.set(None) /> })}
    </Show>
  }
}

/// Why the user can't add sources and whom to ask (issue #764): an admin role nobody signed yet
/// says so; everybody else learns which workspace admins can.
fn who_adds_sources(i18n: crate::i18n::I18n, state: crate::state::AppState) -> String {
  if let Some(reason) = unsigned_reason(i18n, state) {
    return reason;
  }
  let admins = state
    .read(|backend| backend.owner_roster())
    .map(|status| signed_admin_ids(&status))
    .unwrap_or_default();
  if admins.is_empty() {
    return i18n.t("channel-sources-admins-only");
  }
  let names = admins
    .iter()
    .map(|user| {
      state
        .read(|backend| backend.user(*user))
        .map_or_else(|| user.to_string(), |found| found.display_name)
    })
    .collect::<Vec<_>>()
    .join(", ");
  i18n.t_with("channel-sources-ask-admins", &[("admins", &names)])
}

/// Everybody with admin rights in the signed roster: owners, then admins.
fn signed_admin_ids(status: &teamcraft_client_api::admin::OwnerRosterStatus) -> Vec<crate::model::UserId> {
  status
    .owners
    .iter()
    .map(|owner| owner.user)
    .chain(status.admins.iter().copied())
    .collect()
}

/// A source just added, with its token: shown once, kept only while this panel is open. The token
/// is zeroized when the panel closes (the last reference goes).
#[derive(Clone)]
struct Revealed {
  name: String,
  kind: SourceKind,
  token: Arc<SourceToken>,
}

impl PartialEq for Revealed {
  fn eq(&self, other: &Self) -> bool {
    Arc::ptr_eq(&self.token, &other.token)
  }
}

#[component]
fn SourceRow(
  conversation: ConversationId,
  source: IntegrationSource,
  can_manage: Signal<bool>,
  maybe_bot: Memo<Option<UserId>>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let is_confirming = RwSignal::new(false);
  let id = source.id;
  let is_own = source.created_by == state.me().id;
  let can_revoke = move || can_manage.get() || is_own;
  let creator = state
    .read(|backend| backend.user(source.created_by))
    .map(|user| user.display_name)
    .unwrap_or_default();
  let added = crate::clock::format_date(source.created_at_ms, i18n.locale().tag());
  let kind_key = kind_label(&source.kind);
  let name = source.name.clone();
  let has_picture = source.maybe_picture_sha256.is_some();
  let maybe_source_picture = source.maybe_picture_sha256.map(|sha256| (conversation, sha256));
  let avatar = {
    let source = source.clone();
    // Read once, untracked: the avatar follows the stand-in bot only.
    let me = untrack(|| state.me());
    move || {
      let (preview, shows_profile_picture) = source_preview(me.clone(), &source, maybe_bot.get());
      view! {
        <Avatar
          user=preview
          size=AvatarSize::Medium
          maybe_source_picture=maybe_source_picture
          shows_profile_picture=shows_profile_picture
        />
      }
    }
  };
  let is_renaming = RwSignal::new(false);
  let is_editing_name = {
    let name = name.clone();
    move || {
      is_renaming.get().then(|| {
        view! {
          <RenameSource conversation=conversation source=id name=name.clone() on_done=move || is_renaming.set(false) />
        }
      })
    }
  };
  view! {
    <li class="settings-member">
      {avatar}
      <span class="grow source-meta">
        <span translate="no">{name}</span>
        <span class="muted-text">
          {move || i18n.t_with("channel-sources-added-by", &[("name", &creator), ("date", &added)])}
        </span>
      </span>
      <span class="badge">{move || i18n.t(kind_key)}</span>
      <Show when=move || can_manage.get() && !is_renaming.get() fallback=|| ()>
        <button type="button" class="btn btn-small" on:click=move |_| is_renaming.set(true)>
          {move || i18n.t("channel-sources-rename")}
        </button>
      </Show>
      <Show when=can_revoke fallback=|| ()>
        <Show
          when=move || is_confirming.get()
          fallback=move || {
            view! {
              <button type="button" class="btn btn-small btn-quiet" on:click=move |_| is_confirming.set(true)>
                {move || i18n.t("channel-sources-revoke")}
              </button>
            }
          }
        >
          <button
            type="button"
            class="btn btn-small btn-danger"
            on:click=move |_| state.write(|backend| backend.revoke_integration_source(conversation, id))
          >
            {move || i18n.t("channel-sources-revoke-confirm")}
          </button>
          <button type="button" class="btn btn-small" on:click=move |_| is_confirming.set(false)>
            {move || i18n.t("cancel")}
          </button>
        </Show>
      </Show>
      {is_editing_name}
      <Show when=move || can_manage.get() fallback=|| ()>
        <SourcePicture conversation=conversation source=id has_picture=has_picture />
      </Show>
    </li>
  }
}

/// The bot whose picture stands in for the channel's sources without one: the channel's only bot.
/// With several it isn't known which one posts for them.
fn stand_in_bot(bots: &[UserId]) -> Option<UserId> {
  match bots {
    [bot] => Some(*bot),
    _ => None,
  }
}

/// What a source's row shows when the source has no picture, as its messages do (docs/bridge.md,
/// "Source identity"): the profile picture of `maybe_bot`, the bot that posts for the source, else
/// the source's initials in its own colour. `me` only fills the fields the avatar doesn't show, and
/// whether the profile picture may show (the second value) is off without a bot: never the picture
/// of whoever added the source or views it (issue #1299); the adder is named in the text.
fn source_preview(me: User, source: &IntegrationSource, maybe_bot: Option<UserId>) -> (User, bool) {
  let preview = User {
    id: maybe_bot.unwrap_or(me.id),
    display_name: source.name.clone(),
    avatar_hue: source.name.bytes().fold(0_u16, |hue, byte| {
      (hue.wrapping_mul(31).wrapping_add(u16::from(byte))) % 360
    }),
    ..me
  };
  (preview, maybe_bot.is_some())
}

/// Renames a source: an encrypted control message like adding one, so it also works offline.
/// Messages sent from then on show the new name.
#[component]
fn RenameSource(
  conversation: ConversationId,
  source: SourceId,
  name: String,
  #[prop(into)] on_done: Callback<()>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let field_id = format!("channel-source-rename-{source}");
  let container_id = format!("{field_id}-field");
  let draft = RwSignal::new(name);
  let errors = FormErrors::new();
  let pending = PendingAction::new();
  let found_errors = {
    let field_id = field_id.clone();
    move || {
      let mut found = FieldErrors::new();
      if !draft.with(|draft| is_valid_source_name(draft)) {
        let max = MAX_SOURCE_NAME_CHARS.to_string();
        found.push(
          field_id.clone(),
          i18n.t_with("channel-sources-name-invalid", &[("max", &max)]),
        );
      }
      found
    }
  };
  Effect::new({
    let found_errors = found_errors.clone();
    move |_| errors.follow(found_errors())
  });
  let save = {
    let container_id = container_id.clone();
    move |_| {
      let found = untrack(&found_errors);
      if !found.is_empty() {
        errors.refuse(container_id.clone(), found);
        return;
      }
      errors.clear();
      let new_name = draft.with_untracked(|draft| draft.trim().to_owned());
      pending.run_and_then(
        || state.write(|backend| backend.rename_integration_source(conversation, source, new_name)),
        move || on_done.run(()),
      );
    }
  };
  view! {
    <div id=container_id class="field source-rename">
      <label for=field_id.clone()>{move || i18n.t("channel-sources-name")}</label>
      <input
        id=field_id.clone()
        class="input"
        type="text"
        maxlength=MAX_SOURCE_NAME_CHARS.to_string()
        aria-required="true"
        aria-invalid=errors.invalid(field_id.clone())
        aria-describedby=errors.described(field_id.clone())
        prop:value=move || draft.get()
        on:input=move |ev| draft.set(event_target_value(&ev))
      />
      <FieldError errors=errors field=field_id />
      <div class="integration-copy-row">
        <PendingButton pending=pending class="btn btn-small btn-primary" on_click=save>
          {move || i18n.t("channel-sources-rename-save")}
        </PendingButton>
        <button type="button" class="btn btn-small" on:click=move |_| on_done.run(())>
          {move || i18n.t("cancel")}
        </button>
      </div>
      <p class="hint">{move || i18n.t("channel-sources-rename-hint")}</p>
    </div>
  }
}

/// Sets or removes a source's picture. The picture is cropped and scaled here, and travels in an
/// encrypted control message; the server never sees it (e2ee.md §13.1).
#[component]
fn SourcePicture(conversation: ConversationId, source: SourceId, has_picture: bool) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let field_id = format!("channel-source-picture-{source}");
  let picked = RwSignal::new_local(None::<web_sys::File>);
  let maybe_error = RwSignal::new(None::<&'static str>);
  let removing = PendingAction::new();
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
        maybe_error.set(None);
        picked.set(Some(file));
      },
      Err(error) => maybe_error.set(Some(error.i18n_key())),
    }
  };
  let apply = Callback::new(move |png: Vec<u8>| -> CropUpload {
    Box::pin(async move {
      let Some(picture) = fit_source_picture(&png, CROPPED_SOURCE_PICTURE_PX) else {
        maybe_error.set_if_alive(Some("channel-sources-picture-too-large"));
        picked.set_if_alive(None);
        return Err(ActionFailure::new(
          FailureKind::Other,
          "the picture does not fit a source picture",
        ));
      };
      state.write(|backend| backend.set_integration_source_picture(conversation, source, Some(picture)));
      picked.set_if_alive(None);
      Ok(())
    })
  });
  let remove = move |_| {
    removing
      .run(|| state.write(|backend| backend.set_integration_source_picture(conversation, source, None)));
  };
  let has_error = move || maybe_error.with(Option::is_some);
  let error_id = crate::form_errors::error_id(&field_id);
  let described = error_id.clone();
  view! {
    <div
      id=field_id
      class="source-picture-actions"
      role="group"
      tabindex="-1"
      aria-label=move || i18n.t("channel-sources-picture")
      aria-invalid=move || has_error().then_some("true")
      aria-describedby=move || has_error().then(|| described.clone())
    >
      <label class="btn btn-small">
        <Icon name=IconName::Camera />
        <span>{move || i18n.t(if has_picture { "channel-sources-picture-change" } else { "channel-sources-picture-set" })}</span>
        <input type="file" class="visually-hidden" accept="image/png,image/jpeg,image/webp" on:change=pick />
      </label>
      {has_picture.then(|| view! {
        <PendingButton pending=removing class="btn btn-small btn-quiet" on_click=remove>
          {move || i18n.t("channel-sources-picture-remove")}
        </PendingButton>
      })}
      {move || maybe_error.get().map(|key| view! {
        <p id=error_id.clone() class="field-error" role="alert">{move || i18n.t(key)}</p>
      })}
    </div>
    {move || picked.get().map(|file| view! {
      <ImageCropDialog
        file=file
        target=CropTarget::IntegrationPicture
        on_apply=apply
        on_cancel=Callback::new(move |()| picked.set(None))
      />
    })}
  }
}

/// Name, kind and (for webhooks) whether danger attachments alert, then "Add source".
#[component]
fn AddSource(
  conversation: ConversationId,
  has_bot: Signal<bool>,
  #[prop(into)] on_added: Callback<Revealed>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let name = RwSignal::new(String::new());
  let kind = RwSignal::new(SourceKind::Webhook);
  let is_danger_alert = RwSignal::new(true);
  let maybe_error: RwSignal<Option<&'static str>> = RwSignal::new(None);
  let trimmed = move || name.with(|name| name.trim().to_owned());
  let errors = FormErrors::new();
  let name_errors = move || {
    let mut found = FieldErrors::new();
    if !is_valid_source_name(&trimmed()) {
      let max = MAX_SOURCE_NAME_CHARS.to_string();
      found.push(
        SOURCE_NAME_ID,
        i18n.t_with("channel-sources-name-invalid", &[("max", &max)]),
      );
    }
    found
  };
  Effect::new(move |_| errors.follow(name_errors()));
  let can_add = move || has_bot.get();
  let add = move || {
    let found = untrack(name_errors);
    if !found.is_empty() {
      errors.refuse(SOURCE_FIELD_ID, found);
      return;
    }
    errors.clear();
    let Some(new_source) = NewSource::generate() else {
      maybe_error.set(Some("channel-sources-no-random"));
      return;
    };
    maybe_error.set(None);
    let source_name = trimmed();
    let source_kind = kind.get_untracked();
    let options = SourceOptions {
      danger_is_alert: source_kind == SourceKind::Webhook && is_danger_alert.get_untracked(),
      ..SourceOptions::default()
    };
    let announcement = NewIntegrationSource {
      id: SourceId::random(),
      name: source_name.clone(),
      kind: source_kind.clone(),
      token_sha256: new_source.token_sha256,
      options,
    };
    state.write(|backend| backend.add_integration_source(conversation, announcement));
    name.set(String::new());
    on_added.run(Revealed {
      name: source_name,
      kind: source_kind,
      token: Arc::new(new_source.token),
    });
  };
  let kind_button = move |value: SourceKind, key: &'static str| {
    let is_selected = Signal::derive({
      let selected = value.clone();
      move || kind.with(|kind| *kind == selected)
    });
    view! {
      <button
        type="button"
        role="radio"
        aria-checked=move || is_selected.get().to_string()
        aria-pressed=move || is_selected.get().to_string()
        on:click=move |_| kind.set(value.clone())
      >
        {move || i18n.t(key)}
      </button>
    }
  };
  view! {
    <div id=SOURCE_FIELD_ID class="field">
      <label for=SOURCE_NAME_ID>{move || i18n.t("channel-sources-name")}</label>
      <input
        id=SOURCE_NAME_ID
        class="input"
        type="text"
        maxlength=MAX_SOURCE_NAME_CHARS.to_string()
        aria-required="true"
        aria-invalid=errors.invalid(SOURCE_NAME_ID)
        aria-describedby=errors.described(SOURCE_NAME_ID)
        placeholder=move || i18n.t("channel-sources-name-placeholder")
        prop:value=move || name.get()
        on:input=move |ev| name.set(event_target_value(&ev))
        on:keydown=move |ev| {
          if ev.key() == "Enter" && can_add() {
            // Enter adds the source; it never submits a surrounding form.
            ev.prevent_default();
            add();
          }
        }
      />
      <FieldError errors=errors field=SOURCE_NAME_ID />
    </div>
    <div class="segmented" role="radiogroup" aria-label=move || i18n.t("channel-sources-kind")>
      {kind_button(SourceKind::Webhook, "channel-sources-kind-webhook")}
      {kind_button(SourceKind::Log, "channel-sources-kind-log")}
    </div>
    <Show when=move || kind.with(|kind| *kind == SourceKind::Webhook) fallback=|| ()>
      <label class="checkbox">
        <input
          type="checkbox"
          prop:checked=move || is_danger_alert.get()
          on:change=move |ev| is_danger_alert.set(event_target_checked(&ev))
        />
        <span>{move || i18n.t("channel-sources-danger-alert")}</span>
      </label>
    </Show>
    <div class="integration-copy-row">
      <button type="button" class="btn btn-small btn-primary" prop:disabled=move || !can_add() on:click=move |_| add()>
        <Icon name=IconName::Plus />
        {move || i18n.t("channel-sources-add")}
      </button>
    </div>
    <p class="hint">
      {move || if has_bot.get() { i18n.t("channel-sources-hint") } else { i18n.t("channel-sources-needs-bot") }}
    </p>
    {move || maybe_error.get().map(|key| view! { <p class="field-error" role="alert">{i18n.t(key)}</p> })}
  }
}

/// The new source's URL, shown once with a copy button and an example, on the instance's host
/// name (teamcraft-deploy) or on the bridge's loopback default.
#[component]
fn RevealSource(revealed: Revealed, #[prop(into)] on_done: Callback<()>) -> impl IntoView {
  let i18n = use_i18n();
  let server = web_origin().unwrap_or_default();
  let location = RwSignal::new(BridgeLocation::Instance);
  let token = StoredValue::new(Arc::clone(&revealed.token));
  let kind = revealed.kind.clone();
  let is_webhook = kind == SourceKind::Webhook;
  let url = Memo::new(move |_| {
    let base = bridge_base_url(location.get(), &server);
    token.with_value(|token| {
      if is_webhook {
        webhook_url(&base, token.expose_secret())
      } else {
        ingest_url(&base, token.expose_secret())
      }
    })
  });
  let example = Signal::derive(move || {
    let url = url.get();
    if is_webhook {
      format!("curl -d '{{\"text\":\"Deploy finished\"}}' {url}")
    } else {
      format!("journalctl -f | curl -T - -H 'Content-Type: text/plain' {url}")
    }
  });
  let url_text = Signal::derive(move || url.get());
  let location_button = move |value: BridgeLocation, key: &'static str| {
    let is_selected = move || location.get() == value;
    view! {
      <button
        type="button"
        role="radio"
        aria-checked=move || is_selected().to_string()
        aria-pressed=move || is_selected().to_string()
        on:click=move |_| location.set(value)
      >
        {move || i18n.t(key)}
      </button>
    }
  };
  let name = revealed.name.clone();
  view! {
    <p class="hint" role="status">{move || i18n.t_with("channel-sources-created", &[("name", &name)])}</p>
    <div class="segmented" role="radiogroup" aria-label=move || i18n.t("channel-sources-bridge")>
      {location_button(BridgeLocation::Instance, "channel-sources-bridge-instance")}
      {location_button(BridgeLocation::Loopback, "channel-sources-bridge-loopback")}
    </div>
    <p class="hint">
      {move || match location.get() {
        BridgeLocation::Instance => i18n.t("channel-sources-bridge-instance-hint"),
        BridgeLocation::Loopback => i18n.t("channel-sources-bridge-loopback-hint"),
      }}
    </p>
    <label for="channel-source-url">{move || i18n.t("channel-sources-url")}</label>
    <input id="channel-source-url" class="input mono" readonly prop:value=url_text />
    <div class="integration-copy-row"><CopyButton text=url_text /></div>
    <label for="channel-source-example">{move || i18n.t("channel-sources-example")}</label>
    <input id="channel-source-example" class="input mono" readonly prop:value=example />
    <div class="integration-copy-row"><CopyButton text=example /></div>
    <p class="hint">{move || i18n.t("channel-sources-shown-once")}</p>
    <div class="integration-copy-row">
      <button type="button" class="btn btn-small" on:click=move |_| on_done.run(())>
        {move || i18n.t("channel-sources-done")}
      </button>
    </div>
  }
}

fn kind_label(kind: &SourceKind) -> &'static str {
  match kind {
    SourceKind::Webhook => "channel-sources-kind-webhook",
    SourceKind::Log => "channel-sources-kind-log",
    SourceKind::Unknown(_) => "channel-sources-kind-other",
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{DeviceSetup, PresenceStatus, id_from_raw};

  const ME: u64 = 1;
  const BOT: u64 = 2;
  const OTHER_BOT: u64 = 3;

  #[test]
  fn only_the_channels_one_bot_stands_in() {
    assert_eq!(stand_in_bot(&[]), None);
    assert_eq!(stand_in_bot(&[user(BOT)]), Some(user(BOT)));
    assert_eq!(
      stand_in_bot(&[user(BOT), user(OTHER_BOT)]),
      None,
      "unknown which one posts"
    );
  }

  #[test]
  fn a_source_without_picture_shows_the_bots_picture_else_its_initials_never_the_viewers() {
    let source = source();
    let (with_bot, shows_bots) = source_preview(me(), &source, Some(user(BOT)));
    assert_eq!(with_bot.id, user(BOT));
    assert!(shows_bots);
    assert_eq!(with_bot.display_name, "Hotline", "the initials are the source's");

    let (without_bot, shows_mine) = source_preview(me(), &source, None);
    assert!(!shows_mine, "the viewer's own picture never stands in");
    assert_eq!(without_bot.display_name, "Hotline");
  }

  fn source() -> IntegrationSource {
    IntegrationSource {
      id: SourceId::random(),
      name: "Hotline".into(),
      kind: SourceKind::Webhook,
      created_by: user(ME),
      created_at_ms: 0,
      maybe_picture_sha256: None,
    }
  }

  fn me() -> User {
    User {
      id: user(ME),
      username: "me".into(),
      display_name: "Me".into(),
      presence: PresenceStatus::Online,
      maybe_custom_status: None,
      avatar_hue: 0,
      is_guest: false,
      is_bot: false,
      maybe_organization: None,
      device_setup: DeviceSetup::Unknown,
      is_deactivated: false,
      is_deleted: false,
      maybe_email: None,
    }
  }

  fn user(raw: u64) -> UserId {
    id_from_raw(raw)
  }
}
