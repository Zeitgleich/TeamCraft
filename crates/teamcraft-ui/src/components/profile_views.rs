//! Profiles: the card of a person (name, presence, custom status, role, local time, what the user
//! shares with them), the custom status line other views reuse, and the helpers the profile form
//! (Settings > Account, `settings_account.rs`) shares with it.

use leptos::prelude::*;
use teamcraft_proto::{UserId, WorkingHours};

use crate::calls::{CallKind, CallRequest};
use crate::clock::{
  end_of_local_day, format_date_time, format_relative, format_relative_short, format_time, local_day, now_ms,
  time_in_zone,
};
use crate::components::avatar::{Avatar, AvatarSize, PresenceDot};
use crate::components::icons::{Icon, IconName};
use crate::components::message_actions::copy_to_clipboard;
use crate::components::modal::Modal;
use crate::i18n::{I18n, use_i18n};
use crate::model::{
  Conversation, ConversationId, ConversationKind, CustomStatus, PresenceStatus, Space, SpaceId, TimestampMs,
  User,
};
use crate::profiles::use_profiles;
use crate::state::{Overlay, use_app_state};

pub use teamcraft_proto::Weekday;

/// Below this, "last seen" reads "just now": seconds are noise, and the label doesn't tick.
const LAST_SEEN_JUST_NOW_MS: i64 = 2 * 60_000;
/// Shared channels the card lists before it sums up the rest ("and 4 more").
const MAX_SHARED_CHANNELS: usize = 8;

/// `HH:MM` (what `<input type=time>` holds) to minutes since midnight.
pub fn parse_minutes(text: &str) -> Option<u16> {
  let (hours, minutes) = text.split_once(':')?;
  let (hours, minutes): (u16, u16) = (hours.parse().ok()?, minutes.parse().ok()?);
  (hours < 24 && minutes < 60).then_some(hours * 60 + minutes)
}

pub fn format_minutes(minutes: u16) -> String {
  format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// "In a meeting" next to a person who is in a call right now (from the server's call rosters).
#[component]
pub fn InMeetingBadge(user: UserId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  view! {
    <Show when=move || state.read(|backend| backend.is_in_call(user)) fallback=|| ()>
      <span class="in-meeting-badge" title=move || i18n.t("presence-in-meeting")>
        <Icon name=IconName::Phone />
        <span class="visually-hidden">{move || i18n.t("presence-in-meeting")}</span>
      </span>
    </Show>
  }
}

/// "Last seen 5 minutes ago" for a person who is offline and lets others see it. Shows nothing
/// while they are online, hide it, appear offline or the workspace switched last seen off.
/// `is_compact` abbreviates it ("last seen: 5 min. ago") for the line under a name.
#[component]
pub fn LastSeen(user: UserId, #[prop(optional)] is_compact: bool) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let locale = i18n.locale().tag();
  view! {
    {move || {
      let is_offline = state
        .read(|backend| backend.user(user))
        .is_some_and(|person| person.presence == PresenceStatus::Offline);
      let maybe_at = is_offline.then(|| state.read(|backend| backend.last_seen(user))).flatten();
      maybe_at.map(|at| {
        let now = now_ms();
        let is_just_now = now - at < LAST_SEEN_JUST_NOW_MS;
        let label = match (is_compact, is_just_now) {
          (false, true) => i18n.t("last-seen-just-now"),
          (false, false) => i18n.t_with("last-seen", &[("when", &format_relative(at, now, locale))]),
          (true, true) => i18n.t("last-seen-short-just-now"),
          (true, false) => i18n.t_with("last-seen-short", &[("when", &format_relative_short(at, now, locale))]),
        };
        let exact = format_date_time(at, locale);
        view! { <span class="last-seen" title=exact>{label}</span> }
      })
    }}
  }
}

/// A person's card: picture, name and handle, presence and custom status first, then the ways to
/// reach them (message, call, e-mail), what their profile says about their role and hours, and the
/// spaces and channels the user shares with them. Opened from a name or picture anywhere (message
/// author, member list, the header of a direct conversation). A centred dialog on wide screens, a
/// sheet from the bottom on a phone (`.modal-profile` in style/main/440-profile-card.css).
#[component]
pub fn UserProfileDialog(user: UserId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let person: Option<User> = state.read(|backend| backend.user(user));
  let Some(person) = person else {
    return view! { <Modal labelled_by="profile-card-title"><p id="profile-card-title">{move || i18n.t("profile-unavailable")}</p></Modal> }.into_any();
  };
  let is_me = person.id == state.me().id;
  let name = person.display_name.clone();
  let handle = Some(person.username.clone()).filter(|username| !username.is_empty());
  let shared = (!is_me).then(|| {
    state.read(|backend| {
      let spaces = backend.spaces();
      let channels: Vec<Conversation> = spaces
        .iter()
        .flat_map(|space| backend.space_conversations(space.id))
        .collect();
      shared_places(&spaces, &channels, user)
    })
  });
  view! {
    <Modal labelled_by="profile-card-title" class="modal-small modal-profile">
      <button type="button" class="icon-btn profile-close" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
        <Icon name=IconName::Close />
      </button>
      <div class="modal-body profile-card">
        <header class="profile-hero">
          <Avatar user=person.clone() size=AvatarSize::ExtraLarge show_presence=true />
          <h2 id="profile-card-title" class="profile-name" translate="no">{name}</h2>
          {handle.map(|handle| view! { <p class="profile-handle" translate="no">"@" {handle}</p> })}
          <p class="profile-presence">
            <PresenceDot presence=person.presence />
            <span>{move || i18n.t(&format!("presence-{}", person.presence.slug()))}</span>
            <InMeetingBadge user=user />
            <LastSeen user=user is_compact=true />
          </p>
          <ProfileBadges person=person.clone() />
        </header>
        {person.maybe_custom_status.clone().map(|status| view! { <CustomStatusLine status=status class="profile-status" /> })}
        <ProfileActions person=person.clone() is_me=is_me />
        <ProfileFacts user=user maybe_email=person.maybe_email.clone() />
        {shared.map(|shared| view! { <SharedPlacesList shared=shared /> })}
      </div>
    </Modal>
  }
  .into_any()
}

/// Guest and organization badges under the name.
#[component]
fn ProfileBadges(person: User) -> impl IntoView {
  let i18n = use_i18n();
  let state = use_app_state();
  let is_guest_shown = person.is_guest && state.shows_guest_markers();
  let has_badges = is_guest_shown || person.maybe_organization.is_some();
  has_badges.then(|| {
    view! {
      <p class="profile-badges">
        {is_guest_shown.then(|| view! { <span class="badge badge-guest">{move || i18n.t("badge-guest")}</span> })}
        {person.maybe_organization.clone().map(|name| view! { <crate::components::org_badge::OrgBadge organization=Some(name) /> })}
      </p>
    }
  })
}

/// Message, call and copy e-mail for another person; set status and edit profile for oneself.
#[component]
fn ProfileActions(person: User, is_me: bool) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let user = person.id;
  if is_me {
    let can_set_status = state.read(|backend| backend.presence_policy()).statuses_enabled;
    return view! {
      <div class="profile-actions">
        {can_set_status.then(|| view! {
          <button type="button" class="btn btn-primary" data-autofocus on:click=move |_| state.overlay.set(Overlay::CustomStatus)>
            <Icon name=IconName::Smile />
            <span>{move || i18n.t("status-custom-set")}</span>
          </button>
        })}
        <button type="button" class="btn" on:click=move |_| state.open_settings_at(crate::state::SettingsTab::Account)>
          <Icon name=IconName::User />
          <span>{move || i18n.t("profile-edit-title")}</span>
        </button>
      </div>
    }
    .into_any();
  }
  // A call needs the direct conversation; without one yet, "Message" creates it first.
  let maybe_direct = state.direct_with(&[user]);
  let can_message = crate::channel_access::can_start_direct(state, &[user]);
  let can_call = maybe_direct.is_some() && !person.is_bot;
  let is_copied = RwSignal::new(false);
  let maybe_email = person.maybe_email.clone();
  view! {
    <div class="profile-actions">
      <button
        type="button"
        class="btn btn-primary"
        data-autofocus
        disabled=!can_message
        title=move || (!can_message).then(|| i18n.t("profile-message-guest-blocked"))
        on:click=move |_| { state.close_overlay(); state.open_direct(user); }
      >
        <Icon name=IconName::Compose />
        <span>{move || i18n.t("profile-message")}</span>
      </button>
      {(!can_message).then(|| view! { <p class="small muted-text">{move || i18n.t("profile-message-guest-blocked")}</p> })}
      {maybe_direct.filter(|_| can_call).map(|conversation| view! {
        <button
          type="button"
          class="btn"
          on:click=move |_| {
            state.close_overlay();
            state.open_conversation(conversation);
            if state.read(|backend| backend.has_calls()) {
              state.call.prepare(CallRequest { conversation, kind: CallKind::Direct }, false);
            } else {
              state.overlay.set(Overlay::CallsUnavailable);
            }
          }
        >
          <Icon name=IconName::Phone />
          <span>{move || i18n.t("profile-call")}</span>
        </button>
      })}
      {maybe_email.map(|email| view! {
        <button
          type="button"
          class="btn"
          on:click=move |_| {
            copy_to_clipboard(&email);
            is_copied.set(true);
          }
        >
          <Icon name=move || if is_copied.get() { IconName::Check } else { IconName::Copy } />
          <span>{move || if is_copied.get() { i18n.t("profile-email-copied") } else { i18n.t("profile-copy-email") }}</span>
        </button>
      })}
    </div>
  }
  .into_any()
}

/// Role, contact and time facts from the person's profile, plus the e-mail address where the
/// workspace's directory policy shows it (the server leaves it out otherwise).
#[component]
fn ProfileFacts(user: UserId, maybe_email: Option<String>) -> impl IntoView {
  let i18n = use_i18n();
  let locale = i18n.locale().tag();
  let profile = move || use_profiles().and_then(|hub| hub.profile(user));
  move || {
    let maybe_profile = profile();
    let email = maybe_email.clone();
    if maybe_profile.is_none() && email.is_none() {
      return None;
    }
    let (title, department, phone, zone, hours) = match maybe_profile {
      Some(profile) => (
        profile.maybe_title,
        profile.maybe_department,
        profile.maybe_phone,
        profile.maybe_time_zone,
        profile.maybe_working_hours,
      ),
      None => (None, None, None, None, None),
    };
    let local = zone
      .as_deref()
      .and_then(|zone| time_in_zone(now_ms(), zone, locale));
    Some(view! {
      <dl class="profile-facts">
        {title.map(|value| view! { <dt>{move || i18n.t("profile-title")}</dt><dd translate="no">{value}</dd> })}
        {department.map(|value| view! { <dt>{move || i18n.t("profile-department")}</dt><dd translate="no">{value}</dd> })}
        {email.map(|value| {
          let href = format!("mailto:{value}");
          view! { <dt>{move || i18n.t("profile-email")}</dt><dd translate="no"><a href=href>{value}</a></dd> }
        })}
        {phone.map(|value| view! { <dt>{move || i18n.t("profile-phone")}</dt><dd translate="no">{value}</dd> })}
        {local.map(|time| view! { <dt>{move || i18n.t("profile-local-time")}</dt><dd translate="no">{time} " · " {zone.clone().unwrap_or_default()}</dd> })}
        {hours.map(|hours| view! {
          <dt>{move || i18n.t("profile-hours")}</dt>
          <dd>{working_hours_text(&hours, |key| i18n.t(key))}</dd>
        })}
      </dl>
    })
  }
}

/// The spaces and channels the user shares with a person, from the user's own lists.
#[component]
fn SharedPlacesList(shared: SharedPlaces) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let SharedPlaces { spaces, channels } = shared;
  let more = channels.len().saturating_sub(MAX_SHARED_CHANNELS);
  (!spaces.is_empty() || !channels.is_empty()).then(|| {
    view! {
      <section class="profile-shared" aria-labelledby="profile-shared-title">
        <h3 id="profile-shared-title" class="profile-section-title">{move || i18n.t("profile-shared")}</h3>
        {(!spaces.is_empty()).then(|| view! {
          <ul class="profile-chips" aria-label=move || i18n.t("profile-shared-spaces")>
            {spaces.into_iter().map(|(_, name)| view! { <li class="profile-chip" translate="no">{name}</li> }).collect_view()}
          </ul>
        })}
        {(!channels.is_empty()).then(|| view! {
          <ul class="profile-channels" aria-label=move || i18n.t("profile-shared-channels")>
            {channels
              .into_iter()
              .take(MAX_SHARED_CHANNELS)
              .map(|channel| {
                let icon = if channel.kind == ConversationKind::Private { IconName::Lock } else { IconName::Hash };
                let id = channel.id;
                view! {
                  <li>
                    <button type="button" class="profile-channel" on:click=move |_| { state.close_overlay(); state.open_conversation(id); }>
                      <Icon name=icon />
                      <span translate="no">{channel.name}</span>
                    </button>
                  </li>
                }
              })
              .collect_view()}
          </ul>
        })}
        {(more > 0).then(|| view! {
          <p class="profile-more">{move || i18n.t_with("profile-shared-more", &[("count", &more.to_string())])}</p>
        })}
      </section>
    }
  })
}

/// Emoji, text and "until …" of a custom status, in the profile card, the hover card and the
/// account menu.
#[component]
pub fn CustomStatusLine(status: CustomStatus, #[prop(optional)] class: &'static str) -> impl IntoView {
  let i18n = use_i18n();
  let maybe_until = status
    .maybe_expires_at
    .map(|at| status_until_label(at, now_ms(), i18n));
  view! {
    <p class=format!("custom-status-line {class}")>
      {(!status.emoji.is_empty()).then(|| view! { <span class="custom-status-emoji" aria-hidden="true">{status.emoji.clone()}</span> })}
      <span class="custom-status-text" translate="no">{status.text.clone()}</span>
      {maybe_until.map(|label| view! { <span class="custom-status-until">{label}</span> })}
    </p>
  }
}

/// "until 18:00" today, "until end of today" at local midnight, else with the date.
pub fn status_until_label(at: TimestampMs, now: TimestampMs, i18n: I18n) -> String {
  let locale = i18n.locale().tag();
  if at == end_of_local_day(now) {
    return i18n.t("status-until-today");
  }
  let time = if local_day(at) == local_day(now) {
    format_time(at, locale)
  } else {
    format_date_time(at, locale)
  };
  i18n.t_with("status-until", &[("time", &time)])
}

/// What the user shares with a person: spaces (by name) and channels, each sorted by name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SharedPlaces {
  pub spaces: Vec<(SpaceId, String)>,
  pub channels: Vec<SharedChannel>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SharedChannel {
  pub id: ConversationId,
  pub name: String,
  pub kind: ConversationKind,
}

/// The channels among `channels` that have `user` as a member, and the spaces they are in.
pub fn shared_places(spaces: &[Space], channels: &[Conversation], user: UserId) -> SharedPlaces {
  let mut shared: Vec<SharedChannel> = channels
    .iter()
    .filter(|channel| matches!(channel.kind, ConversationKind::Public | ConversationKind::Private))
    .filter(|channel| channel.member_ids.contains(&user))
    .map(|channel| SharedChannel {
      id: channel.id,
      name: channel.name.clone(),
      kind: channel.kind,
    })
    .collect();
  shared.sort_by_key(|channel| channel.name.to_lowercase());
  let mut shared_spaces: Vec<(SpaceId, String)> = spaces
    .iter()
    .filter(|space| {
      channels.iter().any(|channel| {
        channel.maybe_space == Some(space.id) && shared.iter().any(|found| found.id == channel.id)
      })
    })
    .map(|space| (space.id, space.name.clone()))
    .collect();
  shared_spaces.sort_by_key(|(_, name)| name.to_lowercase());
  SharedPlaces {
    spaces: shared_spaces,
    channels: shared,
  }
}

/// "Mon–Fri 09:00–17:00" style summary in the viewer's language.
pub fn working_hours_text(hours: &WorkingHours, translate: impl Fn(&str) -> String) -> String {
  let days: Vec<String> = crate::prefs::EVERY_DAY
    .iter()
    .filter(|day| hours.weekdays.contains(day))
    .map(|day| translate(&format!("weekday-{}", day.as_str())))
    .collect();
  format!(
    "{}, {}–{}",
    days.join(" "),
    format_minutes(hours.start_minute),
    format_minutes(hours.end_minute)
  )
}

/// The other person's local time for a direct conversation header, e.g. "14:32 Europe/Berlin".
pub fn local_time_label(user: UserId, locale: &'static str) -> Option<String> {
  let profile = use_profiles()?.profile(user)?;
  let zone = profile.maybe_time_zone?;
  let time = time_in_zone(now_ms(), &zone, locale)?;
  Some(format!("{time} · {zone}"))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::id_from_raw;

  #[test]
  fn times_round_trip_and_reject_nonsense() {
    assert_eq!(parse_minutes("09:30"), Some(570));
    assert_eq!(format_minutes(570), "09:30");
    assert_eq!(parse_minutes("24:00"), None);
    assert_eq!(parse_minutes("9"), None);
  }

  #[test]
  fn shared_places_are_the_person_s_channels_and_their_spaces_by_name() {
    let (me, other) = (id_from_raw::<UserId>(1), id_from_raw::<UserId>(2));
    let spaces = [space(10, "Zeta"), space(11, "Alpha"), space(12, "Unshared")];
    let channels = [
      channel(20, Some(10), "random", ConversationKind::Public, &[me, other]),
      channel(21, Some(11), "Design", ConversationKind::Private, &[me, other]),
      channel(22, Some(12), "solo", ConversationKind::Public, &[me]),
      channel(23, None, "dm", ConversationKind::Direct, &[me, other]),
    ];
    let shared = shared_places(&spaces, &channels, other);
    let names: Vec<&str> = shared
      .channels
      .iter()
      .map(|channel| channel.name.as_str())
      .collect();
    assert_eq!(names, ["Design", "random"]);
    let space_names: Vec<&str> = shared.spaces.iter().map(|(_, name)| name.as_str()).collect();
    assert_eq!(space_names, ["Alpha", "Zeta"]);
  }

  #[test]
  fn working_hours_read_as_a_sentence() {
    let hours = WorkingHours {
      weekdays: vec![Weekday::Friday, Weekday::Monday],
      start_minute: 540,
      end_minute: 1020,
    };
    let text = working_hours_text(&hours, |key| key.trim_start_matches("weekday-").to_owned());
    assert_eq!(text, "mon fri, 09:00–17:00");
  }

  fn space(id: u64, name: &str) -> Space {
    Space {
      id: id_from_raw(id),
      name: name.to_owned(),
      hue: 0,
      maybe_color: None,
      maybe_image_version: None,
      unread_count: 0,
      mention_count: 0,
      maybe_description: None,
      member_count: 0,
      join_policy: crate::model::JoinPolicy::InviteOnly,
      can_add_members: false,
      can_invite_guests: false,
      can_manage_space: false,
      can_create_channels: false,
      maybe_my_role: None,
    }
  }

  fn channel(
    id: u64,
    maybe_space: Option<u64>,
    name: &str,
    kind: ConversationKind,
    members: &[UserId],
  ) -> Conversation {
    Conversation {
      id: id_from_raw(id),
      maybe_space: maybe_space.map(id_from_raw),
      kind,
      category: crate::model::SidebarCategory::Channels,
      name: name.to_owned(),
      topic: String::new(),
      member_ids: members.to_vec(),
      unread_count: 0,
      mention_count: 0,
      is_favorite: false,
      is_muted: false,
      can_post: true,
      bookmarks: Vec::new(),
      is_catching_up: false,
      is_notes_to_self: false,
      maybe_permissions: None,
      has_only_deactivated_others: false,
      maybe_image_version: None,
    }
  }
}

/// The small card that appears when the pointer rests on a person's name: picture, status, local
/// time and a button for a direct message. Keyboard users open the full card with Enter instead.
#[component]
pub fn UserHoverCard(
  user: UserId,
  /// Fixed position (top, left) in pixels.
  top: f64,
  left: f64,
  on_enter: Callback<()>,
  on_leave: Callback<()>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let locale = i18n.locale().tag();
  let Some(person) = state.read(|backend| backend.user(user)) else {
    return ().into_any();
  };
  let is_me = person.id == state.me().id;
  let name = person.display_name.clone();
  let label = name.clone();
  let local_time = move || {
    let profile = use_profiles().and_then(|hub| hub.profile(user))?;
    let zone = profile.maybe_time_zone?;
    time_in_zone(now_ms(), &zone, locale)
  };
  view! {
    <div
      class="hover-card"
      role="dialog"
      aria-label=label
      style:top=format!("{top}px")
      style:left=format!("{left}px")
      on:mouseenter=move |_| on_enter.run(())
      on:mouseleave=move |_| on_leave.run(())
    >
      <div class="hover-card-head">
        <Avatar user=person.clone() size=AvatarSize::Large show_presence=true />
        <div>
          <strong translate="no">{name}</strong>
          {person.maybe_custom_status.clone().map(|status| view! { <CustomStatusLine status=status class="small" /> })}
          {move || local_time().map(|time| view! { <p class="small muted-text">{move || i18n.t_with("hover-card-local-time", &[("time", &time)])}</p> })}
        </div>
      </div>
      <div class="hover-card-actions">
        {(!is_me && crate::channel_access::can_start_direct(state, &[user])).then(|| view! {
          <button class="btn btn-small btn-primary" on:click=move |_| { on_leave.run(()); state.open_direct(user); }>
            {move || i18n.t("profile-message")}
          </button>
        })}
        <button class="btn btn-small" on:click=move |_| { on_leave.run(()); state.overlay.set(crate::state::Overlay::UserProfile(user)); }>
          {move || i18n.t("hover-card-profile")}
        </button>
      </div>
    </div>
  }
  .into_any()
}
