//! Finding spaces and bringing people in: browsing spaces (joining open ones), inviting people to
//! a space (from the directory, by e-mail, or with the link of an open space) and adding people to
//! a channel. The core and the server decide what the user may do; space membership is directory
//! data the server keeps, channel membership goes through the channel's encryption group
//! (e2ee.md §4.6). These dialogs only collect the choices.

use std::collections::HashSet;

use leptos::prelude::*;
use teamcraft_client_api::actions::SpaceInvite;
use teamcraft_proto::{HistoryPolicy, Role};

use crate::components::icons::{Icon, IconName};
use crate::components::message_actions::copy_to_clipboard;
use crate::components::people_picker::PeoplePicker;
use crate::components::space_tile::SpaceTile;
use crate::components::workspace_dialogs::{Dialog, FailureNote};
use crate::feedback::{ActionFailure, PendingAction, failure_message};
use crate::i18n::use_i18n;
use crate::model::{ConversationId, JoinPolicy, JoinableSpace, Space, SpaceId, User, UserId};
use crate::state::{Overlay, use_app_state};

/// More addresses than this in one go are most likely a pasted list by mistake.
const MAX_EMAILS_PER_INVITE: usize = 50;

/// What the invited people become in the space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InviteRole {
  Member,
  /// Sees only what they are added to; guest accounts can only be guests.
  Guest,
}

/// The e-mail addresses typed into the invitation field.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct EmailList {
  valid: Vec<String>,
  invalid: Vec<String>,
}

/// What the invitation dialog reports after sending.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InviteStatus {
  Editing,
  Sent { count: usize },
}

impl InviteRole {
  /// The role a new invitation starts with: member when the user may add members (a guest
  /// invitation limits what the person sees, so it has to be a deliberate choice).
  fn default_for(space: &Space) -> Option<Self> {
    if space.can_add_members {
      Some(Self::Member)
    } else if space.can_invite_guests {
      Some(Self::Guest)
    } else {
      None
    }
  }

  fn is_guest(self) -> bool {
    self == Self::Guest
  }

  /// Whether a person from the directory can get this role: guest accounts only become guests,
  /// bots are added by admins elsewhere.
  fn fits(self, user: &User) -> bool {
    !user.is_bot && (self == Self::Guest || !user.is_guest)
  }
}

impl EmailList {
  /// Splits on commas, semicolons and white space, lowercases and drops repeats. The check is a
  /// plausibility check only; the server and the mail server have the last word.
  fn parse(text: &str) -> Self {
    let mut list = Self::default();
    let mut seen = HashSet::new();
    for part in text.split(|c: char| c == ',' || c == ';' || c.is_whitespace()) {
      let address = part
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_lowercase();
      if address.is_empty() || !seen.insert(address.clone()) {
        continue;
      }
      if is_plausible_email(&address) {
        list.valid.push(address);
      } else {
        list.invalid.push(address);
      }
    }
    list
  }

  fn is_too_long(&self) -> bool {
    self.valid.len() > MAX_EMAILS_PER_INVITE
  }
}

fn is_plausible_email(address: &str) -> bool {
  address.split_once('@').is_some_and(|(local, domain)| {
    !local.is_empty()
      && domain.contains('.')
      && !domain.starts_with('.')
      && !domain.ends_with('.')
      && !domain.contains('@')
  })
}

/// The address list without `address` (any spelling `EmailList::parse` accepts), the rest joined
/// with commas.
fn without_address(text: &str, address: &str) -> String {
  let list = EmailList::parse(text);
  list
    .valid
    .into_iter()
    .chain(list.invalid)
    .filter(|candidate| candidate != address)
    .collect::<Vec<_>>()
    .join(", ")
}

/// The address of an open space for colleagues to open and join: the web app's path of the space
/// under the server's origin. Only http(s) origins make a link someone else can open.
fn space_link(origin: &str, space: SpaceId) -> Option<String> {
  let origin = origin.trim_end_matches('/');
  (origin.starts_with("https://") || origin.starts_with("http://"))
    .then(|| format!("{origin}{}", crate::route::Route::Space(space).path()))
}

/// Where the server's web app lives: the desktop's account server, else the configured server,
/// else the page's own origin (the web app served by the server).
pub(crate) fn web_origin() -> Option<String> {
  if let Some(server) = crate::session::use_session().and_then(|session| session.server.get_untracked()) {
    return Some(server.origin);
  }
  let configured = crate::auth::AppConfig::load().server_url;
  if !configured.is_empty() {
    return Some(configured);
  }
  web_sys::window().and_then(|window| window.location().origin().ok())
}

/// Spaces matching the filter (name or description, any case), by name.
fn matching<T>(mut items: Vec<T>, filter: &str, text: impl Fn(&T) -> (String, Option<String>)) -> Vec<T> {
  let needle = filter.trim().to_lowercase();
  items.retain(|item| {
    let (name, maybe_description) = text(item);
    needle.is_empty()
      || name.to_lowercase().contains(&needle)
      || maybe_description.is_some_and(|description| description.to_lowercase().contains(&needle))
  });
  items.sort_by_key(|item| text(item).0.to_lowercase());
  items
}

/// Open spaces to join and the user's own spaces, with the way to create a new one. Invite-only
/// spaces appear once someone invites the user: invitations apply when they sign in.
#[component]
pub fn BrowseSpacesDialog(maybe_highlight: Option<SpaceId>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let filter = RwSignal::new(String::new());
  let is_guest = Memo::new(move |_| state.read(|backend| backend.my_role()) == Role::Guest);
  let joinable = Memo::new(move |_| {
    let mut list = matching(
      state.read(|backend| backend.joinable_spaces()),
      &filter.get(),
      |space| (space.name.clone(), space.maybe_description.clone()),
    );
    // A shared link opens this dialog on its space.
    if let Some(index) = maybe_highlight.and_then(|id| list.iter().position(|space| space.id == id)) {
      let linked = list.remove(index);
      list.insert(0, linked);
    }
    list
  });
  let mine = Memo::new(move |_| {
    matching(state.read(|backend| backend.spaces()), &filter.get(), |space| {
      (space.name.clone(), space.maybe_description.clone())
    })
  });
  view! {
    <Dialog title_key="spaces-browse-title">
      <label class="visually-hidden" for="spaces-filter">{move || i18n.t("spaces-browse-filter")}</label>
      <input
        id="spaces-filter"
        class="input"
        data-autofocus
        autocomplete="off"
        placeholder=move || i18n.t("spaces-browse-filter")
        prop:value=move || filter.get()
        on:input=move |ev| filter.set(event_target_value(&ev))
      />
      <Show when=move || !is_guest.get() fallback=|| ()>
        <h3>{move || i18n.t("spaces-browse-open")}</h3>
        <ul class="browse-list">
          <For each=move || joinable.get() key=|space| (space.id, space.member_count, space.maybe_image_version.clone()) let:space>
            <JoinableSpaceRow space=space.clone() is_highlighted=maybe_highlight == Some(space.id) />
          </For>
          <Show when=move || joinable.with(Vec::is_empty) fallback=|| ()>
            <li class="empty-state">{move || i18n.t("spaces-browse-open-none")}</li>
          </Show>
        </ul>
      </Show>
      <h3>{move || i18n.t("spaces-browse-mine")}</h3>
      <ul class="browse-list">
        <For each=move || mine.get() key=|space| (space.id, space.member_count, space.maybe_image_version.clone(), space.can_add_members) let:space>
          <OwnSpaceRow space=space />
        </For>
        <Show when=move || mine.with(Vec::is_empty) fallback=|| ()>
          <li class="empty-state">{move || i18n.t("spaces-browse-mine-none")}</li>
        </Show>
      </ul>
      <p class="hint">{move || i18n.t("spaces-browse-invite-only-hint")}</p>
      <Show when=move || !is_guest.get() fallback=|| ()>
        <footer class="modal-foot">
          <span class="grow"></span>
          <button type="button" class="btn btn-primary" on:click=move |_| state.overlay.set(Overlay::NewSpace)>
            <Icon name=IconName::Plus />
            {move || i18n.t("start-create-space")}
          </button>
        </footer>
      </Show>
    </Dialog>
  }
}

/// The open spaces with a Join button each (also on the getting-started page).
#[component]
pub fn JoinableSpaceList() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let spaces = Memo::new(move |_| state.read(|backend| backend.joinable_spaces()));
  view! {
    <ul class="browse-list">
      <For each=move || spaces.get() key=|space| (space.id, space.member_count, space.maybe_image_version.clone()) let:space>
        <JoinableSpaceRow space=space is_highlighted=false />
      </For>
      <Show when=move || spaces.with(Vec::is_empty) fallback=|| ()>
        <li class="empty-state">{move || i18n.t("spaces-browse-open-none")}</li>
      </Show>
    </ul>
  }
}

#[component]
fn JoinableSpaceRow(space: JoinableSpace, is_highlighted: bool) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let (id, name) = (space.id, space.name.clone());
  view! {
    <li class="browse-row space-row" class:highlighted=is_highlighted>
      <SpaceTile space=space.tile() />
      <SpaceText name=space.name.clone() maybe_description=space.maybe_description.clone() member_count=space.member_count />
      <button
        class="btn btn-small btn-primary"
        on:click=move |_| {
          state.close_overlay();
          state.join_space(id, name.clone());
        }
      >
        {move || i18n.t("space-join")}
      </button>
    </li>
  }
}

#[component]
fn OwnSpaceRow(space: Space) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = space.id;
  // Open spaces can always be shared as a link, so everyone in them gets the invite dialog.
  let can_invite = space.can_add_members || space.can_invite_guests || space.join_policy == JoinPolicy::Open;
  view! {
    <li class="browse-row space-row">
      <SpaceTile space=space.clone() />
      <SpaceText name=space.name.clone() maybe_description=space.maybe_description.clone() member_count=space.member_count />
      {can_invite.then(|| view! {
        <button class="btn btn-small" on:click=move |_| state.overlay.set(Overlay::InvitePeople(id))>
          {move || i18n.t("invite-title")}
        </button>
      })}
      <button class="btn btn-small" on:click=move |_| { state.close_overlay(); state.open_space(id); }>
        {move || i18n.t("browse-open")}
      </button>
    </li>
  }
}

#[component]
fn SpaceText(name: String, maybe_description: Option<String>, member_count: u32) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <div class="browse-text">
      <span class="browse-name" translate="no">{name}</span>
      {maybe_description.map(|description| view! { <span class="browse-topic" translate="no">{description}</span> })}
      // Older servers don't count; nobody is in a space with nobody.
      {(member_count > 0).then(|| view! {
        <span class="browse-meta muted-text">{move || i18n.t_count("members-count", member_count as usize)}</span>
      })}
    </div>
  }
}

/// Invites people to a space: from the directory (they join right away), by e-mail (they join when
/// they first sign in) or, for an open space, with its link.
#[component]
pub fn InvitePeopleDialog(space: SpaceId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let maybe_space = state
    .read(|backend| backend.spaces())
    .into_iter()
    .find(|candidate| candidate.id == space);
  let Some(current) = maybe_space else {
    return view! { <Dialog title_key="invite-title"><p>{move || i18n.t("space-missing")}</p></Dialog> }
      .into_any();
  };
  let maybe_role = InviteRole::default_for(&current);
  let role = RwSignal::new(maybe_role.unwrap_or(InviteRole::Member));
  let has_both_roles = current.can_add_members && current.can_invite_guests;
  let chosen = RwSignal::new(Vec::<User>::new());
  let emails = RwSignal::new(String::new());
  let message = RwSignal::new(String::new());
  let status = RwSignal::new(InviteStatus::Editing);
  // How many of the sent invitations the core hasn't answered yet, and the last refusal (also
  // shown as a toast, issue #322).
  let in_flight = RwSignal::new(0_usize);
  let last_failure = RwSignal::new(None::<ActionFailure>);
  let is_busy = move || in_flight.get() != 0;
  let parsed = Memo::new(move |_| EmailList::parse(&emails.get()));
  // A switch to "member" drops picked guest accounts: they can only be guests.
  Effect::new(move |_| {
    let current_role = role.get();
    chosen.update(|list| list.retain(|user| current_role.fits(user)));
  });
  let has_anyone = move || !chosen.with(Vec::is_empty) || !parsed.with(|list| list.valid.is_empty());
  let is_sendable =
    move || has_anyone() && parsed.with(|list| list.invalid.is_empty() && !list.is_too_long());
  let maybe_link = (current.join_policy == JoinPolicy::Open)
    .then(web_origin)
    .flatten()
    .and_then(|origin| space_link(&origin, space));

  let send = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    if !is_sendable() {
      return;
    }
    let is_guest = role.get_untracked().is_guest();
    let users: Vec<UserId> = chosen.with_untracked(|list| list.iter().map(|user| user.id).collect());
    let addresses = parsed.get_untracked().valid;
    let maybe_message = Some(message.get_untracked().trim().to_owned()).filter(|text| !text.is_empty());
    let count = users.len() + addresses.len();
    status.set(InviteStatus::Editing);
    last_failure.set(None);
    // Each part reports on its own: what went through leaves the form, so "send" again only sends
    // what failed (a second invitation mail must not go out). The dialog may be gone by then,
    // hence `try_set`.
    let finish_one = move |maybe_failure: Option<&ActionFailure>| {
      if let Some(failure) = maybe_failure {
        let _ = last_failure.try_set(Some(failure.clone()));
      }
      let _ = in_flight.try_update(|count| *count = count.saturating_sub(1));
      let is_done = in_flight.try_get_untracked() == Some(0);
      if is_done && last_failure.try_with_untracked(Option::is_none) == Some(true) {
        let _ = status.try_set(InviteStatus::Sent { count });
      }
    };
    if !users.is_empty() {
      in_flight.update(|count| *count += 1);
      // Adding is idempotent on the server, so a retry of the people is harmless.
      PendingAction::new().run_with(
        || state.write(|backend| backend.add_space_members(space, users, is_guest)),
        move |maybe_failure| {
          if maybe_failure.is_none() {
            let _ = chosen.try_set(Vec::new());
          }
          finish_one(maybe_failure);
        },
      );
    }
    for email in addresses {
      in_flight.update(|count| *count += 1);
      let sent = email.clone();
      PendingAction::new().run_with(
        || {
          state.write(|backend| {
            backend.invite_to_space(SpaceInvite {
              space,
              email,
              is_guest,
              channels: Vec::new(),
              maybe_expires_at: None,
              maybe_message: maybe_message.clone(),
            })
          })
        },
        move |maybe_failure| {
          if maybe_failure.is_none() {
            let _ = emails.try_update(|text| *text = without_address(text, &sent));
          }
          finish_one(maybe_failure);
        },
      );
    }
  };

  view! {
    <Dialog title_key="invite-title">
      <p class="muted-text" translate="no">{current.name.clone()}</p>
      {match maybe_role {
        None => view! { <p class="hint">{move || i18n.t("invite-link-only")}</p> }.into_any(),
        Some(_) => view! {
          <form class="invite-form" on:submit=send>
            {has_both_roles.then(|| view! {
              <fieldset class="radio-group">
                <legend>{move || i18n.t("invite-role")}</legend>
                <label class="check-row">
                  <input type="radio" name="invite-role" prop:checked=move || role.get() == InviteRole::Member on:change=move |_| role.set(InviteRole::Member) />
                  <span>{move || i18n.t("invite-role-member")}</span>
                </label>
                <label class="check-row">
                  <input type="radio" name="invite-role" prop:checked=move || role.get() == InviteRole::Guest on:change=move |_| role.set(InviteRole::Guest) />
                  <span>{move || i18n.t("invite-role-guest")}</span>
                </label>
              </fieldset>
            })}
            {(!has_both_roles && maybe_role == Some(InviteRole::Guest)).then(|| view! {
              <p class="hint">{move || i18n.t("invite-guests-only")}</p>
            })}
            <h3>{move || i18n.t("invite-from-directory")}</h3>
            <PeoplePicker chosen=chosen chosen_label_key="invite-chosen" allows_guest_accounts=Signal::derive(move || role.get().is_guest()) />
            <h3>{move || i18n.t("invite-by-email")}</h3>
            <label class="visually-hidden" for="invite-emails">{move || i18n.t("invite-by-email")}</label>
            <textarea
              id="invite-emails"
              class="input"
              rows="2"
              autocomplete="off"
              placeholder=move || i18n.t("invite-email-placeholder")
              prop:value=move || emails.get()
              on:input=move |ev| { status.set(InviteStatus::Editing); emails.set(event_target_value(&ev)) }
            ></textarea>
            <p class="hint">{move || i18n.t("invite-email-hint")}</p>
            {move || {
              let list = parsed.get();
              if !list.invalid.is_empty() {
                let addresses = list.invalid.join(", ");
                view! { <p class="field-error" role="alert">{i18n.t_with("invite-email-invalid", &[("addresses", &addresses)])}</p> }.into_any()
              } else if list.is_too_long() {
                view! { <p class="field-error" role="alert">{i18n.t_with("invite-email-too-many", &[("max", &MAX_EMAILS_PER_INVITE.to_string())])}</p> }.into_any()
              } else {
                ().into_any()
              }
            }}
            <label for="invite-message">{move || i18n.t("invite-message")}</label>
            <textarea
              id="invite-message"
              class="input"
              rows="2"
              maxlength="1000"
              prop:value=move || message.get()
              on:input=move |ev| message.set(event_target_value(&ev))
            ></textarea>
            <p class="hint">{move || i18n.t("invite-message-hint")}</p>
            <footer class="modal-foot">
              // Reported once the core answered without a failure.
              {move || match status.get() {
                InviteStatus::Sent { count } => view! { <span role="status">{i18n.t_count("invite-done", count)}</span> }.into_any(),
                InviteStatus::Editing => view! {
                  {move || last_failure.get().map(|failure| view! { <span class="field-error" role="alert">{failure_message(i18n, &failure)}</span> })}
                }.into_any(),
              }}
              <span class="grow"></span>
              <button type="button" class="btn" on:click=move |_| state.close_overlay()>{move || i18n.t("close")}</button>
              <button
                type="submit"
                class="btn btn-primary"
                prop:disabled=move || is_busy() || !is_sendable()
                aria-busy=move || is_busy().then_some("true")
              >
                <Show when=is_busy fallback=|| ()>
                  <span class="spinner" role="status" aria-label=move || i18n.t("action-running")></span>
                </Show>
                {move || i18n.t("invite-send")}
              </button>
            </footer>
          </form>
        }.into_any(),
      }}
      {maybe_link.map(|link| view! { <SpaceLink link=link /> })}
    </Dialog>
  }
  .into_any()
}

/// The link of an open space with a copy button.
#[component]
fn SpaceLink(link: String) -> impl IntoView {
  let i18n = use_i18n();
  let is_copied = RwSignal::new(false);
  let copied_link = link.clone();
  // Focusing the field selects the link, for copying by hand.
  let select_all = |ev: leptos::ev::FocusEvent| event_target::<web_sys::HtmlInputElement>(&ev).select();
  view! {
    <h3>{move || i18n.t("invite-link")}</h3>
    <div class="inline-form">
      <label class="visually-hidden" for="invite-link">{move || i18n.t("invite-link")}</label>
      <input
        id="invite-link"
        class="input mono"
        readonly
        translate="no"
        prop:value=link
        on:focus=select_all
      />
      <button
        type="button"
        class="btn"
        on:click=move |_| {
          copy_to_clipboard(&copied_link);
          is_copied.set(true);
        }
      >
        <Icon name=IconName::Copy />
        {move || if is_copied.get() { i18n.t("invite-link-copied") } else { i18n.t("invite-link-copy") }}
      </button>
    </div>
    <p class="hint">{move || i18n.t("invite-link-hint")}</p>
  }
}

/// Adds people to a channel through its encryption group; "share history" decides whether they
/// can read what was written before (e2ee.md). Channel members must be in the channel's space: when
/// the user may add people to the space, picked people who aren't in it yet join it first.
#[component]
pub fn AddToChannelDialog(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let chosen = RwSignal::new(Vec::<User>::new());
  let shares_history = RwSignal::new(false);
  let members = Signal::derive(move || {
    state
      .read(|backend| backend.conversation(conversation))
      .map(|info| info.member_ids.into_iter().collect::<HashSet<_>>())
      .unwrap_or_default()
  });
  let maybe_space = state
    .read(|backend| backend.conversation(conversation))
    .and_then(|info| info.maybe_space)
    .and_then(|id| {
      state
        .read(|backend| backend.spaces())
        .into_iter()
        .find(|space| space.id == id)
    });
  let space_id = maybe_space.as_ref().map(|space| space.id);
  let adds_to_space = maybe_space.as_ref().is_some_and(|space| space.can_add_members);
  let joins_space = maybe_space
    .as_ref()
    .is_some_and(|space| space.can_add_members || space.can_invite_guests);
  let pending = PendingAction::new();
  // The dialog stays open until the core answered: it closes on success, and on a failure it
  // shows it and keeps the picked people for another try (those added meanwhile are greyed out).
  let add = move || {
    let in_channel = members.get_untracked();
    let people: Vec<(UserId, bool)> = chosen.with_untracked(|list| {
      list
        .iter()
        .filter(|user| !in_channel.contains(&user.id))
        .map(|user| (user.id, user.is_guest))
        .collect()
    });
    if people.is_empty() {
      return;
    }
    let history = if shares_history.get_untracked() {
      HistoryPolicy::Full
    } else {
      HistoryPolicy::FromJoin
    };
    pending.run_and_then(
      || state.write(|backend| backend.add_people_to_channel(conversation, people, history, joins_space)),
      move || state.close_overlay(),
    );
  };
  view! {
    <Dialog title_key="channel-add-people">
      <form on:submit=move |ev: leptos::ev::SubmitEvent| { ev.prevent_default(); add(); }>
        <PeoplePicker
          chosen=chosen
          chosen_label_key="channel-add-chosen"
          allows_guest_accounts=true
          already_in=members
          already_in_key="channel-already-member"
          on_submit=Callback::new(move |()| add())
        />
        <label class="switch-row">
          <input type="checkbox" role="switch" prop:checked=move || shares_history.get() on:change=move |ev| shares_history.set(event_target_checked(&ev)) />
          <span class="switch-text">
            <strong>{move || i18n.t("channel-share-history")}</strong>
            <small class="muted-text">{move || if shares_history.get() { i18n.t("channel-share-history-on") } else { i18n.t("channel-share-history-off") }}</small>
          </span>
        </label>
        {space_id.map(|space| view! {
          <p class="hint">
            {move || if adds_to_space { i18n.t("channel-add-joins-space") } else { i18n.t("channel-add-space-only") }}
            " "
            <button type="button" class="link-btn" on:click=move |_| state.overlay.set(Overlay::InvitePeople(space))>
              {move || i18n.t("invite-title")}
            </button>
          </p>
        })}
        <FailureNote pending=pending />
        <footer class="modal-foot">
          <span class="grow"></span>
          <button type="button" class="btn" on:click=move |_| state.close_overlay()>{move || i18n.t("cancel")}</button>
          <button
            type="submit"
            class="btn btn-primary"
            prop:disabled=move || pending.is_pending() || chosen.with(Vec::is_empty)
            aria-busy=move || pending.is_pending().then_some("true")
          >
            <Show when=move || pending.is_pending() fallback=|| ()>
              <span class="spinner" role="status" aria-label=move || i18n.t("action-running")></span>
            </Show>
            {move || if pending.maybe_failure().is_some() { i18n.t("data-retry") } else { i18n.t("channel-add") }}
          </button>
        </footer>
      </form>
    </Dialog>
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{DeviceSetup, PresenceStatus, id_from_raw};

  mod emails {
    use super::*;

    #[test]
    fn addresses_are_split_lowercased_and_deduplicated() {
      let list = EmailList::parse("Ann@Example.com, bob@example.org;\n<carol@example.net>  ann@example.com");
      assert_eq!(
        list.valid,
        ["ann@example.com", "bob@example.org", "carol@example.net"]
      );
      assert!(list.invalid.is_empty());
    }

    #[test]
    fn implausible_addresses_are_reported() {
      let list = EmailList::parse("ann@example.com no-at-sign @example.com a@b a@.com a@b.com.");
      assert_eq!(list.valid, ["ann@example.com"]);
      assert_eq!(
        list.invalid,
        ["no-at-sign", "@example.com", "a@b", "a@.com", "a@b.com."]
      );
    }

    #[test]
    fn a_sent_address_leaves_the_field() {
      assert_eq!(
        without_address(
          "Ann@Example.com, bob@example.org\ncarol@example.net",
          "ann@example.com"
        ),
        "bob@example.org, carol@example.net"
      );
      assert_eq!(without_address("ann@example.com", "ann@example.com"), "");
    }

    #[test]
    fn an_empty_field_is_empty() {
      assert_eq!(EmailList::parse("  ,; \n"), EmailList::default());
    }

    #[test]
    fn a_huge_paste_is_too_long() {
      let text = (0..=MAX_EMAILS_PER_INVITE)
        .map(|n| format!("person{n}@example.com"))
        .collect::<Vec<_>>()
        .join(",");
      assert!(EmailList::parse(&text).is_too_long());
    }
  }

  mod roles {
    use super::*;

    #[test]
    fn invitations_are_for_members_by_default() {
      let space = space(true, true);
      assert_eq!(InviteRole::default_for(&space), Some(InviteRole::Member));
    }

    #[test]
    fn without_member_rights_only_guests_can_be_invited() {
      assert_eq!(
        InviteRole::default_for(&space(false, true)),
        Some(InviteRole::Guest)
      );
      assert_eq!(InviteRole::default_for(&space(false, false)), None);
    }

    #[test]
    fn guest_accounts_only_fit_the_guest_role_and_bots_none() {
      let guest = User {
        is_guest: true,
        ..person(2, "Guest")
      };
      let bot = User {
        is_bot: true,
        ..person(3, "Bot")
      };
      assert!(!InviteRole::Member.fits(&guest));
      assert!(InviteRole::Guest.fits(&guest));
      assert!(!InviteRole::Guest.fits(&bot));
      assert!(InviteRole::Member.fits(&person(4, "Member")));
    }
  }

  mod links {
    use super::*;

    #[test]
    fn an_open_space_links_to_its_path_on_the_server() {
      assert_eq!(
        space_link("https://chat.example.com/", id_from_raw(42)).as_deref(),
        Some("https://chat.example.com/s/42")
      );
    }

    #[test]
    fn app_internal_origins_give_no_link() {
      for origin in ["tauri://localhost", "", "null"] {
        assert_eq!(space_link(origin, id_from_raw(42)), None, "{origin}");
      }
    }
  }

  #[test]
  fn spaces_match_name_or_description() {
    let spaces = vec![
      ("Sales".to_owned(), None),
      ("Project A".to_owned(), Some("Sales support".to_owned())),
      ("Ops".to_owned(), None),
    ];
    let found = matching(spaces, "sales", Clone::clone);
    let names: Vec<&str> = found.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["Project A", "Sales"]);
  }

  fn space(can_add_members: bool, can_invite_guests: bool) -> Space {
    Space {
      id: id_from_raw(7),
      name: "Project A".into(),
      hue: 0,
      maybe_color: None,
      maybe_image_version: None,
      unread_count: 0,
      mention_count: 0,
      maybe_description: None,
      member_count: 3,
      join_policy: JoinPolicy::Open,
      can_add_members,
      can_invite_guests,
      can_manage_space: false,
    }
  }

  fn person(id: u64, name: &str) -> User {
    User {
      id: id_from_raw(id),
      username: name.to_lowercase(),
      display_name: name.into(),
      presence: PresenceStatus::Online,
      maybe_custom_status: None,
      avatar_hue: 0,
      is_guest: false,
      is_bot: false,
      maybe_organization: None,
      is_deactivated: false,
      is_deleted: false,
      device_setup: DeviceSetup::Unknown,
      maybe_email: None,
    }
  }
}
