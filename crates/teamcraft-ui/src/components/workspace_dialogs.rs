//! Managing the workspace: new spaces and channels, the channel browser, and channel and space
//! settings (rename, picture and color, categories, privacy conversion, archive). Browsing spaces and
//! inviting people live in `space_invites`. The core and server decide what the user may do; these
//! dialogs only collect the choices.

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_client_api::actions::{
  BrowsableChannel, CategoryChoice, CategoryInfo, ChannelChanges, NewChannel, NewSpace, SpaceChanges,
};
use teamcraft_proto::{CategoryId, HistoryPolicy, IdempotencyKey, SpaceResponse};
use wasm_bindgen::JsCast;

use crate::channel_policy::{ChannelPolicyHub, parse_retention};
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::composer_commands::left_text;
use crate::components::icons::{Icon, IconName};
use crate::components::image_crop_dialog::{
  CropTarget, CropUpload, ImageCropDialog, allow_image_drop, dropped_file,
};
use crate::components::modal::Modal;
use crate::components::section_tabs::{Section, SectionTabs};
use crate::components::space_tile::SpaceTile;
use crate::feedback::PendingAction;
use crate::i18n::use_i18n;
use crate::image_crop::check_pick;
use crate::model::{ConversationId, JoinPolicy, Space, SpaceId, SpaceMember, SpaceRole, User, hue_from_hex};
use crate::reactive::SetIfAlive;
use crate::space_images::use_space_images;
use crate::state::{Overlay, use_app_state};

/// What the color picker shows for a space without a color of its own.
const DEFAULT_SPACE_COLOR: &str = "#3b5fa0";

/// The color picker's state: untouched, a picked color, or back to the derived default.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ColorChoice {
  Unchanged,
  Set(String),
  Default,
}

/// Which picture the settings preview shows: the store's, or the result of an upload or removal
/// that the store hasn't caught up with yet.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ShownImage {
  FromStore,
  Uploaded(String),
  Removed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImageStatus {
  Idle,
  Busy,
  /// The i18n key of the message.
  Failed(&'static str),
  /// A neutral note (the i18n key), e.g. that the demo keeps no pictures.
  Note(&'static str),
}

/// How the history choice of a new channel is picked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HistoryChoice {
  WorkspaceDefault,
  FromJoin,
  Full,
}

impl HistoryChoice {
  fn policy(self) -> Option<HistoryPolicy> {
    match self {
      Self::WorkspaceDefault => None,
      Self::FromJoin => Some(HistoryPolicy::FromJoin),
      Self::Full => Some(HistoryPolicy::Full),
    }
  }

  fn from_value(value: &str) -> Self {
    match value {
      "from_join" => Self::FromJoin,
      "full" => Self::Full,
      _ => Self::WorkspaceDefault,
    }
  }
}

impl ColorChoice {
  /// The `maybe_color` of the update: nothing, the color, or an empty string that clears it.
  fn change(self) -> Option<String> {
    match self {
      Self::Unchanged => None,
      Self::Set(hex) => hue_from_hex(&hex).map(|_| hex),
      Self::Default => Some(String::new()),
    }
  }
}

impl ShownImage {
  fn after(updated: &SpaceResponse) -> Self {
    match &updated.maybe_image {
      Some(image) => Self::Uploaded(image.version.clone()),
      None => Self::Removed,
    }
  }

  fn version(&self, maybe_stored: Option<&str>) -> Option<String> {
    match self {
      Self::FromStore => maybe_stored.map(str::to_owned),
      Self::Uploaded(version) => Some(version.clone()),
      Self::Removed => None,
    }
  }
}

/// Dialog frame: title, close button, scrolling body.
#[component]
pub(crate) fn Dialog(
  title_key: &'static str,
  #[prop(optional, into)] class: &'static str,
  children: Children,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  view! {
    <Modal labelled_by="ws-dialog-title" class=class>
      <header class="modal-head">
        <h2 id="ws-dialog-title">{move || i18n.t(title_key)}</h2>
        <button type="button" class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
          <Icon name=IconName::Close />
        </button>
      </header>
      <div class="modal-body">{children()}</div>
    </Modal>
  }
}

fn non_empty(text: &str) -> Option<String> {
  let trimmed = text.trim();
  (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[component]
pub fn NewSpaceDialog() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let name = RwSignal::new(String::new());
  let description = RwSignal::new(String::new());
  let is_open = RwSignal::new(true);
  // One key per opened dialog: every submit of it is the same user action.
  let key = IdempotencyKey::random();
  let submit = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    let Some(name) = non_empty(&name.get_untracked()) else {
      return;
    };
    state.close_overlay();
    state.create_space(NewSpace {
      name,
      maybe_description: non_empty(&description.get_untracked()),
      is_open: is_open.get_untracked(),
      maybe_idempotency_key: Some(key),
    });
  };
  view! {
    <Dialog title_key="space-new-title" class="modal-small">
      <form on:submit=submit>
        <label for="space-name">{move || i18n.t("space-name")}</label>
        <input id="space-name" class="input" data-autofocus required maxlength="80" prop:value=move || name.get() on:input=move |ev| name.set(event_target_value(&ev)) />
        <label for="space-description">{move || i18n.t("space-description")}</label>
        <textarea id="space-description" class="input" rows="2" prop:value=move || description.get() on:input=move |ev| description.set(event_target_value(&ev))></textarea>
        <label class="check-row">
          <input type="checkbox" prop:checked=move || is_open.get() on:change=move |ev| is_open.set(event_target_checked(&ev)) />
          <span>{move || i18n.t("space-open")}</span>
        </label>
        <footer class="modal-foot">
          <span class="grow"></span>
          <button type="button" class="btn" on:click=move |_| state.close_overlay()>{move || i18n.t("cancel")}</button>
          <button type="submit" class="btn btn-primary" prop:disabled=move || name.get().trim().is_empty()>{move || i18n.t("create")}</button>
        </footer>
      </form>
    </Dialog>
  }
}

#[component]
pub fn NewChannelDialog(space: SpaceId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let name = RwSignal::new(String::new());
  let topic = RwSignal::new(String::new());
  let is_private = RwSignal::new(false);
  let category = RwSignal::new(String::new());
  let history = RwSignal::new(HistoryChoice::WorkspaceDefault);
  let categories = Memo::new(move |_| state.read(|backend| backend.categories(space)));
  // One key per opened dialog: every submit of it is the same user action.
  let key = IdempotencyKey::random();
  let submit = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    let Some(name) = non_empty(&name.get_untracked()) else {
      return;
    };
    let maybe_category = category
      .get_untracked()
      .parse::<CategoryId>()
      .ok()
      .filter(|_| !category.get_untracked().is_empty());
    state.close_overlay();
    state.create_channel(NewChannel {
      space,
      name,
      is_private: is_private.get_untracked(),
      maybe_topic: non_empty(&topic.get_untracked()),
      maybe_category,
      maybe_history: history.get_untracked().policy(),
      maybe_idempotency_key: Some(key),
    });
  };
  view! {
    <Dialog title_key="channel-new-title" class="modal-small">
      <form on:submit=submit>
        <label for="channel-name">{move || i18n.t("channel-name")}</label>
        <input id="channel-name" class="input" data-autofocus required maxlength="80" prop:value=move || name.get() on:input=move |ev| name.set(event_target_value(&ev)) />
        <label for="channel-topic">{move || i18n.t("channel-topic")}</label>
        <input id="channel-topic" class="input" maxlength="250" prop:value=move || topic.get() on:input=move |ev| topic.set(event_target_value(&ev)) />
        <label class="check-row">
          <input type="checkbox" prop:checked=move || is_private.get() on:change=move |ev| is_private.set(event_target_checked(&ev)) />
          <span>{move || i18n.t("channel-private")}</span>
        </label>
        <label for="channel-category">{move || i18n.t("channel-category")}</label>
        <select id="channel-category" class="input" on:change=move |ev| category.set(event_target_value(&ev))>
          <option value="">{move || i18n.t("channel-uncategorized")}</option>
          <For each=move || categories.get() key=|category| category.id let:category>
            <option value=category.id.to_string() translate="no">{category.name.clone()}</option>
          </For>
        </select>
        <label for="channel-history">{move || i18n.t("channel-history")}</label>
        <select id="channel-history" class="input" on:change=move |ev| history.set(HistoryChoice::from_value(&event_target_value(&ev)))>
          <option value="">{move || i18n.t("history-default")}</option>
          <option value="from_join">{move || i18n.t("history-from-join")}</option>
          <option value="full">{move || i18n.t("history-full")}</option>
        </select>
        <footer class="modal-foot">
          <span class="grow"></span>
          <button type="button" class="btn" on:click=move |_| state.close_overlay()>{move || i18n.t("cancel")}</button>
          <button type="submit" class="btn btn-primary" prop:disabled=move || name.get().trim().is_empty()>{move || i18n.t("create")}</button>
        </footer>
      </form>
    </Dialog>
  }
}

/// All public channels of a space, joined or not.
#[component]
pub fn BrowseChannelsDialog(space: SpaceId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let filter = RwSignal::new(String::new());
  let show_archived = RwSignal::new(false);
  let channels = Memo::new(move |_| {
    let needle = filter.get().trim().to_lowercase();
    let mut list = state.read(|backend| backend.browse_channels(space));
    list.retain(|channel| {
      (show_archived.get() || !channel.is_archived)
        && (needle.is_empty() || channel.name.to_lowercase().contains(&needle))
    });
    list.sort_by_key(|channel| channel.name.to_lowercase());
    list
  });
  view! {
    <Dialog title_key="browse-title">
      <label class="visually-hidden" for="browse-filter">{move || i18n.t("browse-filter")}</label>
      <input id="browse-filter" class="input" data-autofocus placeholder=move || i18n.t("browse-filter") prop:value=move || filter.get() on:input=move |ev| filter.set(event_target_value(&ev)) />
      <label class="check-row">
        <input type="checkbox" prop:checked=move || show_archived.get() on:change=move |ev| show_archived.set(event_target_checked(&ev)) />
        <span>{move || i18n.t("browse-archived")}</span>
      </label>
      <ul class="browse-list">
        <For each=move || channels.get() key=|channel| (channel.conversation, channel.is_member, channel.is_archived) let:channel>
          <BrowseRow channel=channel />
        </For>
        <Show when=move || channels.get().is_empty() fallback=|| ()>
          <li class="empty-state">{move || i18n.t("browse-empty")}</li>
        </Show>
      </ul>
    </Dialog>
  }
}

#[component]
fn BrowseRow(channel: BrowsableChannel) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = channel.conversation;
  view! {
    <li class="browse-row">
      <div class="browse-text">
        <span class="browse-name" translate="no"><Icon name=IconName::Hash />{channel.name.clone()}</span>
        {channel.has_external_people.then(|| view! { <span class="badge badge-guest">{move || i18n.t("badge-external")}</span> })}
        {channel.is_archived.then(|| view! { <span class="badge">{move || i18n.t("badge-archived")}</span> })}
        <span class="browse-topic" translate="no">{channel.topic.clone()}</span>
      </div>
      {if channel.is_member {
        view! {
          <button class="btn btn-small" on:click=move |_| { state.open_conversation(id); state.close_overlay(); }>{move || i18n.t("browse-open")}</button>
          <button class="btn btn-small" on:click=move |_| state.leave_with_undo(id, left_text(state, i18n, id))>{move || i18n.t("browse-leave")}</button>
        }
        .into_any()
      } else {
        let space = channel.space;
        view! {
          <button class="btn btn-small" on:click=move |_| state.open_channel_preview(space, id)>
            {move || i18n.t("browse-preview")}
          </button>
          <button class="btn btn-small btn-primary" prop:disabled=channel.is_archived on:click=move |_| state.write(|backend| backend.join_channel(id))>
            {move || i18n.t("browse-join")}
          </button>
        }
        .into_any()
      }}
    </li>
  }
}

/// The sections of the channel settings.
const CHANNEL_GENERAL: Section = Section::new("general", "settings-section-general");
const CHANNEL_MEMBERS: Section = Section::new("members", "settings-section-members");
const CHANNEL_PRIVACY: Section = Section::new("privacy", "settings-section-privacy");
const CHANNEL_ADVANCED: Section = Section::new("advanced", "settings-section-advanced");

/// Settings of one channel in sections: general, members, privacy and retention, advanced. Each
/// section saves on its own.
#[component]
pub fn ChannelSettingsDialog(
  conversation: ConversationId,
  /// The section to open on ("members" from the member list).
  #[prop(optional)]
  maybe_section: Option<&'static str>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let info = state.read(|backend| backend.conversation(conversation));
  let Some(info) = info else {
    return view! { <Dialog title_key="channel-settings-title"><p>{move || i18n.t("channel-missing")}</p></Dialog> }.into_any();
  };
  let sections = vec![
    CHANNEL_GENERAL,
    CHANNEL_MEMBERS,
    CHANNEL_PRIVACY,
    CHANNEL_ADVANCED,
  ];
  let first = maybe_section
    .and_then(|slug| sections.iter().copied().find(|section| section.slug == slug))
    .unwrap_or(CHANNEL_GENERAL);
  let selected = RwSignal::new(first);
  let title = info.name.clone();
  view! {
    <Dialog title_key="channel-settings-title" class="modal-settings">
      <p class="muted-text settings-subject" translate="no">{format!("#{title}")}</p>
      <SectionTabs id_prefix="cs" label_key="channel-settings-title" sections=sections selected=selected let:section>
        {
          let info = info.clone();
          match section.slug {
            "members" => view! { <ChannelMembers conversation=conversation /> }.into_any(),
            "privacy" => view! { <ChannelPrivacy conversation=conversation is_private=matches!(info.kind, crate::model::ConversationKind::Private) /> }.into_any(),
            "advanced" => view! { <ChannelAdvanced conversation=conversation space=info.maybe_space /> }.into_any(),
            _ => view! { <ChannelGeneral conversation=conversation name=info.name.clone() topic=info.topic.clone() /> }.into_any(),
          }
        }
      </SectionTabs>
    </Dialog>
  }
  .into_any()
}

/// Name and topic, saved together.
#[component]
fn ChannelGeneral(conversation: ConversationId, name: String, topic: String) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let name = RwSignal::new(name);
  let topic = RwSignal::new(topic);
  let pending = PendingAction::new();
  let has_saved = RwSignal::new(false);
  let save = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    has_saved.set(true);
    pending.run(|| {
      state.write(|backend| {
        backend.update_channel(
          conversation,
          ChannelChanges {
            maybe_name: non_empty(&name.get_untracked()),
            maybe_topic: Some(topic.get_untracked().trim().to_owned()),
            ..ChannelChanges::default()
          },
        )
      })
    });
  };
  view! {
    <form class="settings-form" on:submit=save>
      <label for="cs-name">{move || i18n.t("channel-name")}</label>
      <input id="cs-name" class="input" data-autofocus required maxlength="80" prop:value=move || name.get() on:input=move |ev| { has_saved.set(false); name.set(event_target_value(&ev)) } />
      <label for="cs-topic">{move || i18n.t("channel-topic")}</label>
      <input id="cs-topic" class="input" maxlength="250" prop:value=move || topic.get() on:input=move |ev| { has_saved.set(false); topic.set(event_target_value(&ev)) } />
      <SaveFooter pending=pending has_saved=has_saved />
    </form>
  }
}

/// The section's save button, and "Saved" once the core confirmed (a failure shows as a toast).
#[component]
pub(crate) fn SaveFooter(pending: PendingAction, has_saved: RwSignal<bool>) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <div class="modal-foot">
      <span role="status" class="muted-text">
        {move || (has_saved.get() && !pending.is_pending() && pending.maybe_failure().is_none()).then(|| i18n.t("settings-saved"))}
      </span>
      <span class="grow"></span>
      <button type="submit" class="btn btn-primary" prop:disabled=move || pending.is_pending() aria-busy=move || pending.is_pending().then_some("true")>
        <Show when=move || pending.is_pending() fallback=|| ()>
          <span class="spinner" role="status" aria-label=move || i18n.t("action-running")></span>
        </Show>
        {move || i18n.t("save")}
      </button>
    </div>
  }
}

/// The channel's members with "Add people" on top and a removal (confirmed first) per person.
#[component]
fn ChannelMembers(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let members = Memo::new(move |_| {
    let ids = state
      .read(|backend| backend.conversation(conversation))
      .map(|info| info.member_ids)
      .unwrap_or_default();
    let mut users: Vec<User> =
      state.read(|backend| ids.into_iter().filter_map(|id| backend.user(id)).collect());
    users.sort_by_key(|user| user.display_name.to_lowercase());
    users
  });
  view! {
    <div class="settings-row">
      <span class="muted-text">{move || i18n.t_count("members-count", members.with(Vec::len))}</span>
      <span class="grow"></span>
      <button type="button" class="btn btn-primary" on:click=move |_| state.overlay.set(Overlay::AddToChannel(conversation))>
        <Icon name=IconName::UserPlus />
        {move || i18n.t("channel-add-people")}
      </button>
    </div>
    <ul class="settings-member-list">
      <For each=move || members.get() key=|user| user.id let:user>
        <ChannelMemberRow conversation=conversation user=user />
      </For>
    </ul>
  }
}

#[component]
fn ChannelMemberRow(conversation: ConversationId, user: User) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = user.id;
  let is_me = id == state.me().id;
  let is_confirming = RwSignal::new(false);
  let pending = PendingAction::new();
  let name = user.display_name.clone();
  view! {
    <li class="settings-member">
      <Avatar user=user.clone() size=AvatarSize::Small show_presence=true />
      <span class="member-name" translate="no">{user.display_name.clone()}</span>
      {user.is_guest.then(|| view! { <span class="badge badge-guest">{move || i18n.t("badge-guest")}</span> })}
      {is_me.then(|| view! { <span class="muted-text">{move || i18n.t("settings-member-you")}</span> })}
      <span class="grow"></span>
      {(!is_me).then(|| view! {
        <Show
          when=move || is_confirming.get()
          fallback=move || view! {
            <button type="button" class="btn btn-small" prop:disabled=move || pending.is_pending() on:click=move |_| is_confirming.set(true)>
              {move || i18n.t("settings-member-remove")}
            </button>
          }
        >
          <span role="alert" class="muted-text">{
            let name = name.clone();
            move || i18n.t_with("settings-member-remove-confirm", &[("name", &name)])
          }</span>
          <button
            type="button"
            class="btn btn-small btn-danger"
            on:click=move |_| {
              is_confirming.set(false);
              // The removal goes through the encryption group (e2ee.md §4.6); refusals show as a toast.
              pending.run(|| state.write(|backend| backend.remove_member(conversation, id)));
            }
          >
            {move || i18n.t("confirm")}
          </button>
          <button type="button" class="btn btn-small" on:click=move |_| is_confirming.set(false)>{move || i18n.t("cancel")}</button>
        </Show>
      })}
    </li>
  }
}

/// Public or private (with a confirmation), and how long messages are kept.
#[component]
fn ChannelPrivacy(conversation: ConversationId, is_private: bool) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let is_confirming_privacy = RwSignal::new(false);
  let convert = move |_| {
    state.write(|backend| {
      backend.update_channel(
        conversation,
        ChannelChanges {
          maybe_is_private: Some(!is_private),
          ..ChannelChanges::default()
        },
      )
    });
    state.close_overlay();
  };
  view! {
    <h3>{move || i18n.t("channel-privacy")}</h3>
    <p class="hint">{move || if is_private { i18n.t("channel-privacy-hint-private") } else { i18n.t("channel-privacy-hint-public") }}</p>
    <Show
      when=move || is_confirming_privacy.get()
      fallback=move || view! { <button type="button" class="btn" on:click=move |_| is_confirming_privacy.set(true)>{move || if is_private { i18n.t("channel-make-public") } else { i18n.t("channel-make-private") }}</button> }
    >
      <p role="alert">{move || if is_private { i18n.t("channel-make-public-confirm") } else { i18n.t("channel-make-private-confirm") }}</p>
      <div class="inline-actions">
        <button type="button" class="btn btn-primary" on:click=convert>{move || i18n.t("confirm")}</button>
        <button type="button" class="btn" on:click=move |_| is_confirming_privacy.set(false)>{move || i18n.t("cancel")}</button>
      </div>
    </Show>
    <ChannelRetention conversation=conversation />
  }
}

/// Category, moving to another space and archiving (confirmed first).
#[component]
fn ChannelAdvanced(conversation: ConversationId, space: Option<SpaceId>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let is_confirming_archive = RwSignal::new(false);
  let categories = Memo::new(move |_| {
    space
      .map(|space| state.read(|backend| backend.categories(space)))
      .unwrap_or_default()
  });
  let change_category = move |ev: leptos::ev::Event| {
    let value = event_target_value(&ev);
    let choice = match value.parse::<CategoryId>() {
      Ok(category) if !value.is_empty() => CategoryChoice::Category { category },
      _ => CategoryChoice::Uncategorized,
    };
    state.write(|backend| {
      backend.update_channel(
        conversation,
        ChannelChanges {
          maybe_category: Some(choice),
          ..ChannelChanges::default()
        },
      )
    });
  };
  let archive = move |_| {
    state.write(|backend| {
      backend.update_channel(
        conversation,
        ChannelChanges {
          maybe_is_archived: Some(true),
          ..ChannelChanges::default()
        },
      )
    });
    state.close_overlay();
  };
  view! {
    {space.is_some().then(|| view! {
      <div class="field">
        <label for="cs-category">{move || i18n.t("channel-move-category")}</label>
        <select id="cs-category" class="input" on:change=change_category>
          <option value="">{move || i18n.t("channel-uncategorized")}</option>
          <For each=move || categories.get() key=|category| category.id let:category>
            <option value=category.id.to_string() translate="no">{category.name.clone()}</option>
          </For>
        </select>
      </div>
    })}
    <MoveChannel conversation=conversation current_space=space />
    <h3>{move || i18n.t("channel-archive")}</h3>
    <p class="hint">{move || i18n.t("channel-archive-hint")}</p>
    <Show
      when=move || is_confirming_archive.get()
      fallback=move || view! { <button type="button" class="btn btn-danger" on:click=move |_| is_confirming_archive.set(true)>{move || i18n.t("channel-archive-action")}</button> }
    >
      <p role="alert">{move || i18n.t("channel-archive-confirm")}</p>
      <div class="inline-actions">
        <button type="button" class="btn btn-danger" on:click=archive>{move || i18n.t("confirm")}</button>
        <button type="button" class="btn" on:click=move |_| is_confirming_archive.set(false)>{move || i18n.t("cancel")}</button>
      </div>
    </Show>
  }
}

/// Moves the channel to another space. Members, history and encryption stay as they are.
#[component]
fn MoveChannel(conversation: ConversationId, current_space: Option<SpaceId>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let targets = Memo::new(move |_| {
    state
      .read(|backend| backend.spaces())
      .into_iter()
      .filter(|space| Some(space.id) != current_space)
      .collect::<Vec<_>>()
  });
  let chosen = RwSignal::new(String::new());
  let is_confirming = RwSignal::new(false);
  let move_now = move |_| {
    if let Ok(space) = chosen.get_untracked().parse::<SpaceId>() {
      state.write(|backend| {
        backend.update_channel(
          conversation,
          ChannelChanges {
            maybe_space: Some(space),
            ..ChannelChanges::default()
          },
        )
      });
      state.close_overlay();
    }
  };

  view! {
    <Show when=move || current_space.is_some() && !targets.get().is_empty() fallback=|| ()>
      <h3>{move || i18n.t("channel-move-space")}</h3>
      <p class="hint">{move || i18n.t("channel-move-space-hint")}</p>
      <div class="inline-form">
        <select
          class="input"
          aria-label=move || i18n.t("channel-move-space")
          on:change=move |ev| {
            chosen.set(event_target_value(&ev));
            is_confirming.set(false);
          }
        >
          <option value="">{move || i18n.t("channel-move-space-choose")}</option>
          <For each=move || targets.get() key=|space| space.id let:space>
            <option value=space.id.to_string() translate="no">{space.name.clone()}</option>
          </For>
        </select>
        <button class="btn" prop:disabled=move || chosen.get().is_empty() || is_confirming.get() on:click=move |_| is_confirming.set(true)>
          {move || i18n.t("channel-move-space-action")}
        </button>
      </div>
      <Show when=move || is_confirming.get() fallback=|| ()>
        <p role="alert">{move || i18n.t("channel-move-space-confirm")}</p>
        <div class="inline-actions">
          <button class="btn btn-primary" on:click=move_now>{move || i18n.t("confirm")}</button>
          <button class="btn" on:click=move |_| is_confirming.set(false)>{move || i18n.t("cancel")}</button>
        </div>
      </Show>
    </Show>
  }
}

/// How long the channel keeps its messages, within the limit the administrators set. The server
/// decides who may change it and refuses values above the limit.
#[component]
fn ChannelRetention(conversation: ConversationId) -> impl IntoView {
  let i18n = use_i18n();
  let Some(hub) = use_context::<ChannelPolicyHub>() else {
    return ().into_any();
  };
  let status = RwSignal::new(None::<teamcraft_proto::RetentionStatusResponse>);
  let days = RwSignal::new(String::new());
  let messages = RwSignal::new(String::new());
  let maybe_error = RwSignal::new(None::<&'static str>);
  let is_saved = RwSignal::new(false);
  spawn_local(async move {
    if let Ok(loaded) = hub.retention(conversation).await {
      days.set_if_alive(
        loaded
          .own
          .maybe_max_age_days
          .map(|value| value.to_string())
          .unwrap_or_default(),
      );
      messages.set_if_alive(
        loaded
          .own
          .maybe_max_messages
          .map(|value| value.to_string())
          .unwrap_or_default(),
      );
      status.set_if_alive(Some(loaded));
    }
  });
  let save = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    is_saved.set(false);
    let setting = match parse_retention(&days.get_untracked(), &messages.get_untracked()) {
      Ok(setting) => setting,
      Err(error) => {
        maybe_error.set(Some(error.message_key()));
        return;
      },
    };
    maybe_error.set(None);
    spawn_local(async move {
      match hub.set_retention(conversation, setting).await {
        Ok(saved) => {
          status.set_if_alive(Some(saved));
          is_saved.set_if_alive(true);
        },
        Err(error) => {
          tracing::warn!(%error, "could not save the channel retention");
          maybe_error.set_if_alive(Some("retention-error-refused"));
        },
      }
    });
  };
  let describe = move |setting: teamcraft_proto::RetentionSetting| match (
    setting.maybe_max_age_days,
    setting.maybe_max_messages,
  ) {
    (None, None) => i18n.t("retention-unlimited"),
    (Some(age), None) => i18n.t_with("retention-summary-days", &[("days", &age.to_string())]),
    (None, Some(count)) => i18n.t_with("retention-summary-messages", &[("count", &count.to_string())]),
    (Some(age), Some(count)) => i18n.t_with(
      "retention-summary-both",
      &[("days", &age.to_string()), ("count", &count.to_string())],
    ),
  };

  view! {
    <Show when=move || status.get().is_some() fallback=|| ()>
      <h3>{move || i18n.t("retention-channel-title")}</h3>
      <p class="hint">{move || i18n.t("retention-channel-hint")}</p>
      <form on:submit=save>
        <label for="cs-retention-days">{move || i18n.t("retention-days")}</label>
        <input id="cs-retention-days" class="input" inputmode="numeric" prop:value=move || days.get()
          on:input=move |ev| days.set(event_target_value(&ev)) />
        <label for="cs-retention-messages">{move || i18n.t("retention-messages")}</label>
        <input id="cs-retention-messages" class="input" inputmode="numeric" prop:value=move || messages.get()
          on:input=move |ev| messages.set(event_target_value(&ev)) />
        <p class="muted-text small">
          {move || status.get().map(|current| {
            i18n.t_with("retention-channel-effective", &[("value", &describe(current.effective))])
          })}
          " "
          {move || status.get().map(|current| {
            let limit = current.limit;
            (limit != teamcraft_proto::RetentionSetting::default()).then(|| {
              i18n.t_with("retention-channel-limit", &[("value", &describe(limit))])
            })
          })}
        </p>
        <p role="alert" class="warn-text">{move || maybe_error.get().map(|key| i18n.t(key))}</p>
        <p role="status">{move || is_saved.get().then(|| i18n.t("retention-channel-saved"))}</p>
        <div class="modal-foot">
          <span class="grow"></span>
          <button type="submit" class="btn">{move || i18n.t("save")}</button>
        </div>
      </form>
    </Show>
  }
  .into_any()
}

/// Mutes the whole space: its channels stop notifying unless they have a setting of their own.
#[component]
fn SpaceMute(space: SpaceId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = space.to_string();
  let is_muted = {
    let id = id.clone();
    move || {
      state
        .settings
        .prefs
        .with(|prefs| prefs.notifications.is_space_muted(&id))
    }
  };
  view! {
    <h3>{move || i18n.t("space-notifications")}</h3>
    <label class="checkbox settings-check">
      <input
        type="checkbox"
        prop:checked=is_muted.clone()
        on:change=move |ev| {
          let muted = event_target_checked(&ev);
          state.settings.prefs.update(|prefs| {
            prefs.notifications.set_space(&id, crate::components::sidebar::space_mute_entry(muted))
          });
        }
      />
      <span>
        {move || i18n.t("space-mute-label")}
        <small class="muted-text">{move || i18n.t("space-mute-hint")}</small>
      </span>
    </label>
  }
}

/// The sections of the space settings.
const SPACE_OVERVIEW: Section = Section::new("overview", "settings-section-overview");
const SPACE_MEMBERS: Section = Section::new("members", "settings-section-members");
const SPACE_CHANNELS: Section = Section::new("channels", "settings-section-channels");
const SPACE_NOTIFICATIONS: Section = Section::new("notifications", "settings-section-notifications");
const SPACE_ADVANCED: Section = Section::new("advanced", "settings-section-advanced");

/// Settings of a space in sections: overview, members, channels and categories, notifications,
/// advanced. Each section saves on its own; what the user may not change is read-only with a hint.
#[component]
pub fn SpaceSettingsDialog(space: SpaceId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let maybe_current = state
    .read(|backend| backend.spaces())
    .into_iter()
    .find(|candidate| candidate.id == space);
  let Some(current) = maybe_current else {
    return view! { <Dialog title_key="space-settings-title"><p>{move || i18n.t("space-missing")}</p></Dialog> }.into_any();
  };
  let sections = vec![
    SPACE_OVERVIEW,
    SPACE_MEMBERS,
    SPACE_CHANNELS,
    SPACE_NOTIFICATIONS,
    SPACE_ADVANCED,
  ];
  let selected = RwSignal::new(SPACE_OVERVIEW);
  // Members and invitations load afresh whenever the settings open.
  state.write(|backend| backend.refresh_space_roster(space));
  let name = current.name.clone();
  view! {
    <Dialog title_key="space-settings-title" class="modal-settings">
      <p class="muted-text settings-subject" translate="no">{name}</p>
      <SpaceSettingsSections space=current sections=sections selected=selected />
    </Dialog>
  }
  .into_any()
}

#[component]
fn SpaceSettingsSections(space: Space, sections: Vec<Section>, selected: RwSignal<Section>) -> impl IntoView {
  view! {
    <SectionTabs id_prefix="ss" label_key="space-settings-title" sections=sections selected=selected let:section>
      {
        let space = space.clone();
        match section.slug {
          "members" => view! { <SpaceMembersSection space=space /> }.into_any(),
          "channels" => view! { <SpaceChannelsSection space=space /> }.into_any(),
          "notifications" => view! { <SpaceMute space=space.id /> }.into_any(),
          "advanced" => view! { <SpaceAdvanced space=space /> }.into_any(),
          _ => view! { <SpaceOverview space=space /> }.into_any(),
        }
      }
    </SectionTabs>
  }
}

/// Picture, name, description, color and who may join.
#[component]
fn SpaceOverview(space: Space) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = space.id;
  let can_edit = space.can_manage_space;
  let name = RwSignal::new(space.name.clone());
  let description = RwSignal::new(space.maybe_description.clone().unwrap_or_default());
  let color = RwSignal::new(ColorChoice::Unchanged);
  let is_open = RwSignal::new(space.join_policy == JoinPolicy::Open);
  let was_open = is_open.get_untracked();
  let picker_value = space
    .maybe_color
    .clone()
    .filter(|color| hue_from_hex(color).is_some())
    .unwrap_or_else(|| DEFAULT_SPACE_COLOR.to_owned());
  let pending = PendingAction::new();
  let has_saved = RwSignal::new(false);
  let save = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    has_saved.set(true);
    let open_now = is_open.get_untracked();
    pending.run(|| {
      state.write(|backend| {
        backend.update_space(
          id,
          SpaceChanges {
            maybe_name: non_empty(&name.get_untracked()),
            // An empty description clears it.
            maybe_description: Some(description.get_untracked().trim().to_owned()),
            maybe_color: color.get_untracked().change(),
            maybe_is_open: (open_now != was_open).then_some(open_now),
            ..SpaceChanges::default()
          },
        )
      })
    });
  };
  let edited = move || has_saved.set(false);
  view! {
    {(!can_edit).then(|| view! { <p class="hint">{move || i18n.t("space-settings-read-only")}</p> })}
    <SpaceImageSection space=id />
    <form class="settings-form" on:submit=save>
      <fieldset class="plain-fieldset" prop:disabled=!can_edit>
        <label for="ss-name">{move || i18n.t("space-name")}</label>
        <input id="ss-name" class="input" required maxlength="80" prop:value=move || name.get() on:input=move |ev| { edited(); name.set(event_target_value(&ev)) } />
        <label for="ss-description">{move || i18n.t("space-description")}</label>
        <textarea id="ss-description" class="input" rows="2" prop:value=move || description.get() on:input=move |ev| { edited(); description.set(event_target_value(&ev)) }></textarea>
        <label for="ss-color">{move || i18n.t("space-color")}</label>
        <div class="inline-form">
          <input
            id="ss-color"
            type="color"
            class="color-input"
            prop:value=move || match color.get() {
              ColorChoice::Set(hex) => hex,
              ColorChoice::Unchanged | ColorChoice::Default => picker_value.clone(),
            }
            on:input=move |ev| { edited(); color.set(ColorChoice::Set(event_target_value(&ev))) }
          />
          <button type="button" class="btn btn-small" on:click=move |_| { edited(); color.set(ColorChoice::Default) }>
            {move || i18n.t("space-color-reset")}
          </button>
        </div>
        <fieldset class="radio-group">
          <legend>{move || i18n.t("space-join-policy")}</legend>
          <label class="check-row radio-explained">
            <input type="radio" name="ss-join" prop:checked=move || is_open.get() on:change=move |_| { edited(); is_open.set(true) } />
            <span>
              <strong>{move || i18n.t("space-join-open")}</strong>
              <small class="muted-text">{move || i18n.t("space-join-open-hint")}</small>
            </span>
          </label>
          <label class="check-row radio-explained">
            <input type="radio" name="ss-join" prop:checked=move || !is_open.get() on:change=move |_| { edited(); is_open.set(false) } />
            <span>
              <strong>{move || i18n.t("space-join-invite-only")}</strong>
              <small class="muted-text">{move || i18n.t("space-join-invite-only-hint")}</small>
            </span>
          </label>
        </fieldset>
      </fieldset>
      {can_edit.then(|| view! { <SaveFooter pending=pending has_saved=has_saved /> })}
    </form>
  }
}

/// The space's members with search, roles and removal, inviting, and pending invitations.
#[component]
fn SpaceMembersSection(space: Space) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = space.id;
  let can_manage = space.can_add_members;
  let filter = RwSignal::new(String::new());
  let members = Memo::new(move |_| {
    let needle = filter.get().trim().to_lowercase();
    state.read(|backend| backend.space_members(id)).map(|mut list| {
      list.retain(|member| needle.is_empty() || member.display_name.to_lowercase().contains(&needle));
      list
    })
  });
  let invites = Memo::new(move |_| state.read(|backend| backend.space_invites(id)));
  view! {
    <div class="settings-row">
      <label class="visually-hidden" for="ss-member-filter">{move || i18n.t("space-members-filter")}</label>
      <input id="ss-member-filter" class="input grow" autocomplete="off" placeholder=move || i18n.t("space-members-filter") prop:value=move || filter.get() on:input=move |ev| filter.set(event_target_value(&ev)) />
      <button type="button" class="btn btn-primary" on:click=move |_| state.overlay.set(Overlay::InvitePeople(id))>
        <Icon name=IconName::UserPlus />
        {move || i18n.t("invite-title")}
      </button>
    </div>
    {(!can_manage).then(|| view! { <p class="hint">{move || i18n.t("space-members-read-only")}</p> })}
    {move || match members.get() {
      None => view! { <p class="muted-text" role="status">{move || i18n.t("space-members-loading")}</p> }.into_any(),
      Some(list) => view! {
        <p class="muted-text">{i18n.t_count("members-count", list.len())}</p>
        <ul class="settings-member-list">
          {list.into_iter().map(|member| view! { <SpaceMemberRow space=id member=member can_manage=can_manage /> }).collect_view()}
        </ul>
      }.into_any(),
    }}
    <Show when=move || !invites.with(Vec::is_empty) fallback=|| ()>
      <h3>{move || i18n.t("space-invites-pending")}</h3>
      <ul class="settings-member-list">
        <For each=move || invites.get() key=|invite| invite.id let:invite>
          <li class="settings-member">
            <Icon name=IconName::Send />
            <span class="member-name" translate="no">{invite.email.clone()}</span>
            <span class="muted-text">{
              let role = invite.role.clone();
              move || i18n.t(role_key(&role))
            }</span>
            <span class="grow"></span>
            {can_manage.then(|| {
              let invite_id = invite.id;
              view! {
                <button type="button" class="btn btn-small" on:click=move |_| state.write(|backend| backend.revoke_space_invite(id, invite_id))>
                  {move || i18n.t("space-invite-revoke")}
                </button>
              }
            })}
          </li>
        </For>
      </ul>
    </Show>
  }
}

/// The i18n key of a space role.
fn role_key(role: &SpaceRole) -> &'static str {
  match role {
    SpaceRole::Guest => "space-role-guest",
    SpaceRole::SpaceAdmin => "space-role-admin",
    SpaceRole::Member | SpaceRole::Unknown(_) => "space-role-member",
  }
}

#[component]
fn SpaceMemberRow(space: SpaceId, member: SpaceMember, can_manage: bool) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let user = member.user;
  let is_me = user == state.me().id;
  let is_confirming = RwSignal::new(false);
  let pending = PendingAction::new();
  let maybe_avatar = state.read(|backend| backend.user(user));
  let name = member.display_name.clone();
  let role = member.role.clone();
  // Guest accounts stay guests; members and space admins can be switched.
  let can_change_role = can_manage && !is_me && !member.is_guest;
  view! {
    <li class="settings-member">
      {maybe_avatar.map(|user| view! { <Avatar user=user size=AvatarSize::Small show_presence=true /> })}
      <span class="member-name" translate="no">{member.display_name.clone()}</span>
      {is_me.then(|| view! { <span class="muted-text">{move || i18n.t("settings-member-you")}</span> })}
      <span class="grow"></span>
      {if can_change_role {
        let is_admin = role == SpaceRole::SpaceAdmin;
        view! {
          <label class="visually-hidden" for=format!("ss-role-{user}")>{move || i18n.t("space-role")}</label>
          <select
            id=format!("ss-role-{user}")
            class="input input-small"
            prop:disabled=move || pending.is_pending()
            on:change=move |ev| {
              let role = if event_target_value(&ev) == "space_admin" { SpaceRole::SpaceAdmin } else { SpaceRole::Member };
              pending.run(|| state.write(|backend| backend.set_space_role(space, user, role)));
            }
          >
            <option value="member" selected=!is_admin>{move || i18n.t("space-role-member")}</option>
            <option value="space_admin" selected=is_admin>{move || i18n.t("space-role-admin")}</option>
          </select>
        }.into_any()
      } else {
        let role = role.clone();
        view! { <span class="badge">{move || i18n.t(role_key(&role))}</span> }.into_any()
      }}
      {(can_manage && !is_me).then(|| view! {
        <Show
          when=move || is_confirming.get()
          fallback=move || view! {
            <button type="button" class="btn btn-small" prop:disabled=move || pending.is_pending() on:click=move |_| is_confirming.set(true)>
              {move || i18n.t("settings-member-remove")}
            </button>
          }
        >
          <span role="alert" class="muted-text">{
            let name = name.clone();
            move || i18n.t_with("space-member-remove-confirm", &[("name", &name)])
          }</span>
          <button
            type="button"
            class="btn btn-small btn-danger"
            on:click=move |_| {
              is_confirming.set(false);
              // The server also removes them from the space's channels; refusals show as a toast.
              pending.run(|| state.write(|backend| backend.remove_space_member(space, user)));
            }
          >
            {move || i18n.t("confirm")}
          </button>
          <button type="button" class="btn btn-small" on:click=move |_| is_confirming.set(false)>{move || i18n.t("cancel")}</button>
        </Show>
      })}
    </li>
  }
}

/// The space's channels with their category, and the categories (create, rename, delete inline).
#[component]
fn SpaceChannelsSection(space: Space) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = space.id;
  let can_edit = space.can_manage_space;
  let new_category = RwSignal::new(String::new());
  let categories = Memo::new(move |_| state.read(|backend| backend.categories(id)));
  // Every channel of the space, joined or not, as the channel browser lists them.
  let channels = Memo::new(move |_| {
    let mut list = state.read(|backend| backend.browse_channels(id));
    list.retain(|channel| !channel.is_archived);
    list.sort_by_key(|channel| channel.name.to_lowercase());
    list
  });
  let add_category = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    if let Some(category) = non_empty(&new_category.get_untracked()) {
      state.write(|backend| backend.create_category(id, &category));
      new_category.set(String::new());
    }
  };
  view! {
    <h3>{move || i18n.t("space-categories")}</h3>
    <p class="hint">{move || i18n.t("space-categories-hint")}</p>
    <ul class="category-list">
      <For each=move || categories.get() key=|category| (category.id, category.name.clone()) let:category>
        <CategoryRow space=id category=category />
      </For>
    </ul>
    {can_edit.then(|| view! {
      <form class="inline-form" on:submit=add_category>
        <label class="visually-hidden" for="ss-new-category">{move || i18n.t("space-category-new")}</label>
        <input id="ss-new-category" class="input" placeholder=move || i18n.t("space-category-new") prop:value=move || new_category.get() on:input=move |ev| new_category.set(event_target_value(&ev)) />
        <button type="submit" class="btn btn-small" prop:disabled=move || new_category.get().trim().is_empty()>
          <Icon name=IconName::Plus />
          {move || i18n.t("create")}
        </button>
      </form>
    })}
    <h3>{move || i18n.t("space-channels")}</h3>
    <ul class="settings-member-list">
      <For each=move || channels.get() key=|channel| (channel.conversation, channel.maybe_category) let:browsed>
        {
          let channel = browsed.conversation;
          let current = browsed.maybe_category.map(|category| category.to_string()).unwrap_or_default();
          view! {
            <li class="settings-member">
              <Icon name=IconName::Hash />
              <span class="member-name" translate="no">{browsed.name.clone()}</span>
              <span class="grow"></span>
              <label class="visually-hidden" for=format!("ss-cat-{channel}")>{move || i18n.t("channel-move-category")}</label>
              <select
                id=format!("ss-cat-{channel}")
                class="input input-small"
                on:change=move |ev| {
                  let value = event_target_value(&ev);
                  let choice = match value.parse::<CategoryId>() {
                    Ok(category) if !value.is_empty() => CategoryChoice::Category { category },
                    _ => CategoryChoice::Uncategorized,
                  };
                  state.write(|backend| backend.update_channel(channel, ChannelChanges { maybe_category: Some(choice), ..ChannelChanges::default() }));
                }
              >
                <option value="" selected=current.is_empty()>{move || i18n.t("channel-uncategorized")}</option>
                {move || categories.get().into_iter().map(|category| {
                  let value = category.id.to_string();
                  let is_current = value == current;
                  view! { <option value=value selected=is_current translate="no">{category.name.clone()}</option> }
                }).collect_view()}
              </select>
            </li>
          }
        }
      </For>
    </ul>
  }
}

/// Archiving, at the bottom and confirmed first.
#[component]
fn SpaceAdvanced(space: Space) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = space.id;
  let is_confirming = RwSignal::new(false);
  let archive = move |_| {
    state.write(|backend| {
      backend.update_space(
        id,
        SpaceChanges {
          maybe_is_archived: Some(true),
          ..SpaceChanges::default()
        },
      )
    });
    state.close_overlay();
  };
  view! {
    <h3>{move || i18n.t("channel-archive")}</h3>
    <p class="hint">{move || i18n.t("space-archive-hint")}</p>
    {if space.can_manage_space {
      view! {
        <Show
          when=move || is_confirming.get()
          fallback=move || view! { <button type="button" class="btn btn-danger" on:click=move |_| is_confirming.set(true)>{move || i18n.t("space-archive-action")}</button> }
        >
          <p role="alert">{move || i18n.t("space-archive-confirm")}</p>
          <div class="inline-actions">
            <button type="button" class="btn btn-danger" on:click=archive>{move || i18n.t("confirm")}</button>
            <button type="button" class="btn" on:click=move |_| is_confirming.set(false)>{move || i18n.t("cancel")}</button>
          </div>
        </Show>
      }.into_any()
    } else {
      view! { <p class="hint">{move || i18n.t("space-settings-read-only")}</p> }.into_any()
    }}
  }
}

/// The space's picture: preview, upload (cropped to a 256×256 square in the crop dialog first) and
/// remove.
#[component]
fn SpaceImageSection(space: SpaceId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  // The demo backend has no server to keep pictures: the crop dialog still works, nothing is saved.
  let maybe_hub = use_space_images();
  let shown = RwSignal::new(ShownImage::FromStore);
  let status = RwSignal::new(ImageStatus::Idle);
  let preview = Memo::new(move |_| {
    state
      .read(|backend| backend.spaces())
      .into_iter()
      .find(|candidate| candidate.id == space)
      .map(|mut current| {
        current.maybe_image_version = shown.get().version(current.maybe_image_version.as_deref());
        current
      })
  });
  let has_image = move || {
    preview.with(|maybe| {
      maybe
        .as_ref()
        .is_some_and(|space| space.maybe_image_version.is_some())
    })
  };

  let picked = RwSignal::new_local(None::<web_sys::File>);
  let open_crop = move |file: web_sys::File| {
    if let Err(error) = check_pick(&file.type_(), file.size()) {
      status.set(ImageStatus::Failed(error.i18n_key()));
      return;
    }
    status.set(ImageStatus::Idle);
    picked.set(Some(file));
  };
  let pick = move |ev: leptos::ev::Event| {
    let Some(input) = ev
      .target()
      .and_then(|target| target.dyn_into::<web_sys::HtmlInputElement>().ok())
    else {
      return;
    };
    let maybe_file = input.files().and_then(|files| files.item(0));
    input.set_value("");
    if let Some(file) = maybe_file {
      open_crop(file);
    }
  };
  let apply = Callback::new(move |png: Vec<u8>| -> CropUpload {
    Box::pin(async move {
      let Some(hub) = maybe_hub else {
        picked.set_if_alive(None);
        status.set_if_alive(ImageStatus::Note("space-image-demo"));
        return Ok(());
      };
      let updated = hub
        .upload(space, png)
        .await
        .map_err(|failure| failure.to_failure())?;
      shown.set_if_alive(ShownImage::after(&updated));
      picked.set_if_alive(None);
      Ok(())
    })
  });
  let cancel_crop = Callback::new(move |()| picked.set(None));
  let remove = move |_| {
    let Some(hub) = maybe_hub else { return };
    status.set(ImageStatus::Busy);
    spawn_local(async move {
      match hub.remove(space).await {
        Ok(updated) => {
          shown.set_if_alive(ShownImage::after(&updated));
          status.set_if_alive(ImageStatus::Idle);
        },
        Err(failure) => status.set_if_alive(ImageStatus::Failed(failure.i18n_key())),
      }
    });
  };

  view! {
    <h3>{move || i18n.t("space-image")}</h3>
    <div class="space-image-row" on:dragover=allow_image_drop on:drop=move |ev| {
      if let Some(file) = dropped_file(&ev) {
        open_crop(file);
      }
    }>
      {move || preview.get().map(|space| view! { <span class="space-image-preview"><SpaceTile space=space /></span> })}
      <div class="space-image-actions">
        <label class="btn btn-small" class:disabled=move || status.get() == ImageStatus::Busy>
          {move || if has_image() { i18n.t("space-image-change") } else { i18n.t("space-image-upload") }}
          <input
            type="file"
            class="visually-hidden"
            accept="image/png,image/jpeg,image/webp"
            prop:disabled=move || status.get() == ImageStatus::Busy
            on:change=pick
          />
        </label>
        <Show when=has_image fallback=|| ()>
          <button type="button" class="btn btn-small" prop:disabled=move || status.get() == ImageStatus::Busy on:click=remove>
            {move || i18n.t("space-image-remove")}
          </button>
        </Show>
      </div>
    </div>
    <p class="hint">{move || i18n.t("space-image-hint")}</p>
    {move || match status.get() {
      ImageStatus::Idle => ().into_any(),
      ImageStatus::Busy => view! { <p class="hint" role="status">{move || i18n.t("space-image-uploading")}</p> }.into_any(),
      ImageStatus::Failed(key) => view! { <p class="field-error" role="alert">{move || i18n.t(key)}</p> }.into_any(),
      ImageStatus::Note(key) => view! { <p class="hint" role="status">{move || i18n.t(key)}</p> }.into_any(),
    }}
    {move || picked.get().map(|file| view! {
      <ImageCropDialog
        file=file
        target=CropTarget::SpacePicture
        on_apply=apply
        on_cancel=cancel_crop
      />
    })}
  }
  .into_any()
}

#[component]
fn CategoryRow(space: SpaceId, category: CategoryInfo) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = category.id;
  let name = RwSignal::new(category.name.clone());
  let original = category.name;
  let rename = move |_| {
    if let Some(new_name) = non_empty(&name.get_untracked()).filter(|new_name| *new_name != original) {
      state.write(|backend| backend.rename_category(space, id, &new_name));
    }
  };
  view! {
    <li class="category-row">
      <input class="input" aria-label=move || i18n.t("space-category-rename") prop:value=move || name.get() on:input=move |ev| name.set(event_target_value(&ev)) on:change=rename />
      <button class="icon-btn" aria-label=move || i18n.t("space-category-delete") on:click=move |_| state.write(|backend| backend.delete_category(space, id))>
        <Icon name=IconName::Trash />
      </button>
    </li>
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn history_choice_maps_to_policy() {
    assert_eq!(HistoryChoice::from_value("").policy(), None);
    assert_eq!(
      HistoryChoice::from_value("full").policy(),
      Some(HistoryPolicy::Full)
    );
    assert_eq!(
      HistoryChoice::from_value("from_join").policy(),
      Some(HistoryPolicy::FromJoin)
    );
  }

  #[test]
  fn blank_text_is_none() {
    assert_eq!(non_empty("  \n"), None);
    assert_eq!(non_empty(" a "), Some("a".to_owned()));
  }

  #[test]
  fn the_color_picker_sends_only_a_deliberate_change() {
    assert_eq!(ColorChoice::Unchanged.change(), None);
    assert_eq!(
      ColorChoice::Set("#2f6fed".into()).change(),
      Some("#2f6fed".into())
    );
    assert_eq!(ColorChoice::Set("javascript:".into()).change(), None);
    assert_eq!(ColorChoice::Default.change(), Some(String::new()));
  }

  #[test]
  fn the_preview_follows_an_upload_until_the_store_catches_up() {
    let mut response = space_response();
    assert_eq!(ShownImage::FromStore.version(Some("old")), Some("old".into()));
    assert_eq!(ShownImage::FromStore.version(None), None);

    response.maybe_image = Some(teamcraft_proto::SpaceImage {
      version: "new".into(),
      size_bytes: 10,
    });
    let uploaded = ShownImage::after(&response);
    assert_eq!(uploaded, ShownImage::Uploaded("new".into()));
    assert_eq!(uploaded.version(Some("old")), Some("new".into()));

    response.maybe_image = None;
    let removed = ShownImage::after(&response);
    assert_eq!(removed, ShownImage::Removed);
    assert_eq!(removed.version(Some("old")), None);
  }

  fn space_response() -> SpaceResponse {
    SpaceResponse {
      id: crate::model::id_from_raw(7),
      name: "Project A".into(),
      maybe_icon: None,
      maybe_color: None,
      maybe_description: None,
      maybe_image: None,
      join_policy: teamcraft_proto::JoinPolicy::InviteOnly,
      is_archived: false,
      maybe_brand_id: None,
      maybe_my_role: None,
      maybe_my_notifications: None,
      member_count: 1,
      can_add_members: true,
      can_invite_guests: true,
      can_manage_space: true,
    }
  }
}
