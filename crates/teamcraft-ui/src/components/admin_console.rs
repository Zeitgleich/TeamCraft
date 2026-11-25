use leptos::prelude::*;

use crate::admin::{AdminChannel, AdminFocus, AdminGroup, AdminSection, AdminUser};
use crate::components::admin_announcement::AnnouncementSection;
use crate::components::admin_audit::AuditSection;
use crate::components::admin_backgrounds::BackgroundsSection;
use crate::components::admin_brands::BrandsSection;
use crate::components::admin_calendar::CalendarSection;
use crate::components::admin_channels::ChannelsSection;
use crate::components::admin_emoji::EmojiSection;
use crate::components::admin_health::HealthSection;
use crate::components::admin_import::ImportSection;
use crate::components::admin_instance::InstanceSection;
use crate::components::admin_integrations::IntegrationsSection;
use crate::components::admin_mail::MailSection;
use crate::components::admin_notetaker::NotetakerSection;
use crate::components::admin_people::PeopleSection;
use crate::components::admin_permissions::PermissionsSection;
use crate::components::admin_retention::RetentionSection;
use crate::components::admin_save::refusal_text;
use crate::components::admin_security::SecuritySection;
use crate::components::admin_settings::SettingsSection;
use crate::components::admin_stickers::StickersSection;
use crate::components::admin_updates::UpdatesPolicySection;
use crate::components::admin_users::UsersSection;
use crate::components::admin_webhooks::WebhooksSection;
use crate::components::icons::{Icon, IconName};
use crate::components::roster_banner::RosterBanner;
use crate::history::back_or_home;
use crate::i18n::use_i18n;
use crate::reactive::{use_debounced, use_interval_while_visible};
use crate::state::use_app_state;

/// How often the console reloads the data of the open section while it is open and the page is
/// visible: admins watch audit entries and device lists change, nobody else needs the requests.
/// Only that section's lists are read (a few requests), never the whole console: the address's rate
/// limit is shared with every colleague behind it (issue #1217, docs/performance.md "Admin console").
pub(crate) const REFRESH_MS: u64 = 30_000;
/// The wait after the last key before the console's search asks the server for people and
/// channels.
const FIND_DEBOUNCE_MS: u64 = 250;
/// Fewer letters match too much to be worth a request.
const FIND_MIN_CHARS: usize = 2;

/// Administration inside the normal client. Everything here is metadata: admins can manage rooms
/// without reading them (admin.md §2).
#[component]
pub fn AdminConsole(section: AdminSection) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  // Backends that read the server fetch fresh data when the console opens (everything the first
  // time, then what this section shows) and while it stays open.
  state.read(|backend| backend.admin_refresh_section(section));
  use_interval_while_visible(REFRESH_MS, move || {
    state.read(|backend| backend.admin_refresh_section(section))
  });
  let maybe_error = Memo::new(move |_| state.read(|backend| backend.admin_error()));
  let alerts = Memo::new(move |_| state.read(|backend| backend.health_alert_count()));

  // The page frame (`SectionHeader`) names the group the open page belongs to.
  provide_context(OpenSection(section));

  view! {
    <section class="admin" aria-label=move || i18n.t("admin-title")>
      <AdminNav section=section alerts=alerts />
      <div class="admin-content">
        // On every page: an admin role nobody signed is refused by other people's apps (#765).
        <RosterBanner />
        {move || {
          maybe_error.get().map(|detail| {
            let text = refusal_text(i18n, Some(detail));
            // Sticky: a save refused far down the page still shows (forms.md, #1054).
            view! { <p class="auth-alert admin-error" role="alert">{text}</p> }
          })
        }}
        {move || match section {
          AdminSection::Channels => view! { <ChannelsSection /> }.into_any(),
          AdminSection::Users => view! { <UsersSection /> }.into_any(),
          AdminSection::Permissions => view! { <PermissionsSection /> }.into_any(),
          AdminSection::Security => view! { <SecuritySection /> }.into_any(),
          AdminSection::People => view! { <PeopleSection /> }.into_any(),
          AdminSection::Announcement => view! { <AnnouncementSection /> }.into_any(),
          AdminSection::Backgrounds => view! { <BackgroundsSection /> }.into_any(),
          AdminSection::Emoji => view! { <EmojiSection /> }.into_any(),
          AdminSection::Stickers => view! { <StickersSection /> }.into_any(),
          AdminSection::Brands => view! { <BrandsSection /> }.into_any(),
          AdminSection::Mail => view! { <MailSection /> }.into_any(),
          AdminSection::Calendar => view! { <CalendarSection /> }.into_any(),
          AdminSection::Settings => view! { <SettingsSection /> }.into_any(),
          AdminSection::Health => view! { <HealthSection /> }.into_any(),
          AdminSection::Instance => view! { <InstanceSection /> }.into_any(),
          AdminSection::Retention => view! { <RetentionSection /> }.into_any(),
          AdminSection::Updates => view! { <UpdatesPolicySection /> }.into_any(),
          AdminSection::Integrations => view! { <IntegrationsSection /> }.into_any(),
          AdminSection::Webhooks => view! { <WebhooksSection /> }.into_any(),
          AdminSection::Notetaker => view! { <NotetakerSection /> }.into_any(),
          AdminSection::Import => view! { <ImportSection /> }.into_any(),
          AdminSection::Audit => view! { <AuditSection /> }.into_any(),
        }}
      </div>
    </section>
  }
}

/// The section the console shows, for the page frame below it.
#[derive(Clone, Copy)]
struct OpenSection(AdminSection);

/// The console's navigation (issue #958): the sections under a handful of headings, a field that
/// finds a setting by name or by what it is about, and on a phone a grouped picker instead of a
/// strip of 21 tabs.
#[component]
fn AdminNav(section: AdminSection, alerts: Memo<usize>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let query = RwSignal::new(String::new());
  // What the typed words match, in navigation order; every section while the field is empty.
  let matches = Memo::new(move |_| {
    let query = query.get();
    AdminSection::ALL
      .into_iter()
      .filter(|target| {
        let texts = [
          i18n.t(&format!("admin-nav-{}", target.slug())),
          i18n.t(&format!("admin-nav-{}-keywords", target.slug())),
          group_label(i18n, target.group()),
        ];
        matches_query(&query, &texts)
      })
      .collect::<Vec<_>>()
  });

  // People and channels by name (issue #2360), asked of the server once typing pauses.
  let debounced = use_debounced(query, FIND_DEBOUNCE_MS);
  Effect::new(move |_| {
    let text = debounced.get();
    let asked = if text.trim().chars().count() >= FIND_MIN_CHARS {
      text
    } else {
      String::new()
    };
    state.write(|backend| backend.admin_find(asked));
  });
  on_cleanup(move || {
    // The next page starts with an empty field: the last answer goes with it.
    state.try_write(|backend| backend.admin_find(String::new()));
  });
  let found = Memo::new(move |_| state.read(|backend| backend.admin_found()));
  let is_finding = move || query.with(|text| text.trim().chars().count() >= FIND_MIN_CHARS);
  let open_focus = move |focus: AdminFocus| {
    query.set(String::new());
    state.open_admin_focus(focus);
  };

  let nav_item = move |target: AdminSection| {
    view! {
      <li hidden=move || !matches.with(|found| found.contains(&target))>
        <button
          type="button"
          class="admin-nav-item"
          aria-current=move || (target == section).then_some("page")
          on:click=move |_| state.open_admin(target)
        >
          <Icon name=section_icon(target) />
          <span>{move || i18n.t(&format!("admin-nav-{}", target.slug()))}</span>
          {(target == AdminSection::Health)
            .then(|| {
              view! {
                <Show when=move || { alerts.get() > 0 } fallback=|| ()>
                  <span class="badge-count" role="img" aria-label=move || i18n.t_count("health-alerts-badge", alerts.get())>
                    {move || alerts.get()}
                  </span>
                </Show>
              }
            })}
        </button>
      </li>
    }
  };

  let nav_group = move |group: AdminGroup| {
    let heading_id = format!("admin-nav-group-{}", group.slug());
    let has_match = move || matches.with(|found| found.iter().any(|target| target.group() == group));
    view! {
      <div class="admin-nav-group" hidden=move || !has_match()>
        {(group != AdminGroup::Overview).then(|| {
          let id = heading_id.clone();
          view! { <h2 class="admin-nav-heading" id=id>{move || group_label(i18n, group)}</h2> }
        })}
        <ul aria-labelledby=(group != AdminGroup::Overview).then_some(heading_id.clone())>
          {group.sections().map(nav_item).collect_view()}
        </ul>
      </div>
    }
  };

  // A page far down the list (Server, Integrations) stays in sight when it opens.
  request_animation_frame(|| {
    if let Some(current) = document()
      .query_selector(".admin-nav-groups [aria-current=\"page\"]")
      .ok()
      .flatten()
    {
      let options = web_sys::ScrollIntoViewOptions::new();
      options.set_block(web_sys::ScrollLogicalPosition::Nearest);
      current.scroll_into_view_with_scroll_into_view_options(&options);
    }
  });

  view! {
    <nav class="admin-nav" class:is-finding=move || !query.with(String::is_empty) aria-label=move || i18n.t("admin-nav-label")>
      <button type="button" class="admin-back" on:click=move |_| back_or_home(state)>
        <Icon name=IconName::ChevronRight class="flip" />
        <span>{move || i18n.t("admin-back")}</span>
      </button>
      <h1>{move || i18n.t("admin-title")}</h1>
      <label class="admin-nav-search">
        <Icon name=IconName::Search />
        <span class="visually-hidden">{move || i18n.t("admin-nav-search")}</span>
        <input
          class="input"
          type="search"
          autocomplete="off"
          placeholder=move || i18n.t("admin-nav-search-placeholder")
          prop:value=move || query.get()
          on:input=move |ev| query.set(event_target_value(&ev))
          on:keydown=move |ev| {
            // Enter opens the first page found; Escape empties the field.
            match ev.key().as_str() {
              "Enter" => {
                if let Some(first) = matches.with_untracked(|found| found.first().copied()) {
                  ev.prevent_default();
                  state.open_admin(first);
                } else if let Some(focus) = found.with_untracked(first_focus) {
                  ev.prevent_default();
                  open_focus(focus);
                }
              },
              "Escape" if !query.with_untracked(String::is_empty) => {
                ev.prevent_default();
                query.set(String::new());
              },
              _ => {},
            }
          }
        />
      </label>
      <div class="admin-nav-groups">
        {AdminGroup::ALL.into_iter().map(nav_group).collect_view()}
        <Show when=is_finding fallback=|| ()>
          <FoundEntries found=found on_open=Callback::new(open_focus) />
        </Show>
        <Show
          when=move || {
            matches.with(Vec::is_empty)
              && (!is_finding()
                || found.with(|found| !found.is_searching && found.people.is_empty() && found.channels.is_empty()))
          }
          fallback=|| ()
        >
          <p class="muted-text small admin-nav-empty" role="status">{move || i18n.t("admin-nav-no-match")}</p>
        </Show>
      </div>
      // Phone width: one grouped picker replaces the list (690-compact.css).
      <label class="admin-nav-picker">
        <span class="visually-hidden">{move || i18n.t("admin-nav-label")}</span>
        <select class="input" on:change=move |ev| {
          let slug = event_target_value(&ev);
          if let Some(target) = AdminSection::ALL.into_iter().find(|target| target.slug() == slug) {
            state.open_admin(target);
          }
        }>
          {AdminGroup::ALL
            .into_iter()
            .map(|group| {
              view! {
                <optgroup label=move || group_label(i18n, group)>
                  {group
                    .sections()
                    .map(|target| {
                      view! {
                        <option value=target.slug() selected=target == section>
                          {move || i18n.t(&format!("admin-nav-{}", target.slug()))}
                        </option>
                      }
                    })
                    .collect_view()}
                </optgroup>
              }
            })
            .collect_view()}
        </select>
      </label>
    </nav>
  }
}

/// The people and channels the console's search found, under the pages it found: one click opens
/// the entry on its page.
#[component]
fn FoundEntries(found: Memo<crate::admin::AdminFound>, on_open: Callback<AdminFocus>) -> impl IntoView {
  let i18n = use_i18n();
  let people = Memo::new(move |_| found.with(|found| found.people.clone()));
  let channels = Memo::new(move |_| found.with(|found| found.channels.clone()));
  view! {
    <Show when=move || found.with(|found| found.is_searching) fallback=|| ()>
      <p class="muted-text small admin-nav-empty" role="status">{move || i18n.t("admin-nav-finding")}</p>
    </Show>
    <Show when=move || people.with(|list| !list.is_empty()) fallback=|| ()>
      <div class="admin-nav-group">
        <h2 class="admin-nav-heading" id="admin-nav-found-people">{move || i18n.t("admin-nav-found-people")}</h2>
        <ul aria-labelledby="admin-nav-found-people">
          <For each=move || people.get() key=|entry: &AdminUser| entry.user.id let:entry>
            <li>{person_entry(i18n, entry, on_open)}</li>
          </For>
        </ul>
      </div>
    </Show>
    <Show when=move || channels.with(|list| !list.is_empty()) fallback=|| ()>
      <div class="admin-nav-group">
        <h2 class="admin-nav-heading" id="admin-nav-found-channels">{move || i18n.t("admin-nav-found-channels")}</h2>
        <ul aria-labelledby="admin-nav-found-channels">
          <For each=move || channels.get() key=|channel: &AdminChannel| channel.id let:channel>
            <li>{channel_entry(channel, on_open)}</li>
          </For>
        </ul>
      </div>
    </Show>
  }
}

fn person_entry(i18n: crate::i18n::I18n, entry: AdminUser, on_open: Callback<AdminFocus>) -> impl IntoView {
  let detail_key = if entry.user.is_bot {
    "admin-role-bot"
  } else if entry.user.is_guest {
    "admin-role-guest"
  } else if !entry.is_active {
    "admin-deactivated"
  } else {
    "admin-nav-found-person"
  };
  let focus = AdminFocus::Person {
    id: entry.user.id,
    name: entry.user.display_name.clone(),
    is_bot: entry.user.is_bot,
  };
  view! {
    <button type="button" class="admin-nav-item admin-nav-found" on:click=move |_| on_open.run(focus.clone())>
      <Icon name=if entry.user.is_bot { IconName::Terminal } else { IconName::User } />
      <span class="admin-nav-found-text">
        <span translate="no">{entry.user.display_name.clone()}</span>
        <span class="muted-text small">{move || i18n.t(detail_key)}</span>
      </span>
    </button>
  }
}

fn channel_entry(channel: AdminChannel, on_open: Callback<AdminFocus>) -> impl IntoView {
  let focus = AdminFocus::Channel {
    id: channel.id,
    name: channel.name.clone(),
  };
  let icon = if channel.kind == crate::model::ConversationKind::Private {
    IconName::Lock
  } else {
    IconName::Hash
  };
  view! {
    <button type="button" class="admin-nav-item admin-nav-found" on:click=move |_| on_open.run(focus.clone())>
      <Icon name=icon />
      <span class="admin-nav-found-text">
        <span translate="no">{channel.name.clone()}</span>
        <span class="muted-text small" translate="no">{channel.space_name.clone()}</span>
      </span>
    </button>
  }
}

/// The search hit Enter opens when no page matches: the first person, else the first channel.
fn first_focus(found: &crate::admin::AdminFound) -> Option<AdminFocus> {
  found
    .people
    .first()
    .map(|entry| AdminFocus::Person {
      id: entry.user.id,
      name: entry.user.display_name.clone(),
      is_bot: entry.user.is_bot,
    })
    .or_else(|| {
      found.channels.first().map(|channel| AdminFocus::Channel {
        id: channel.id,
        name: channel.name.clone(),
      })
    })
}

fn group_label(i18n: crate::i18n::I18n, group: AdminGroup) -> String {
  i18n.t(&format!("admin-group-{}", group.slug()))
}

fn section_icon(section: AdminSection) -> IconName {
  match section {
    AdminSection::Health => IconName::Activity,
    AdminSection::Users => IconName::Users,
    AdminSection::People => IconName::User,
    AdminSection::Permissions => IconName::Shield,
    AdminSection::Channels => IconName::Layers,
    AdminSection::Announcement => IconName::Bell,
    AdminSection::Retention => IconName::Clock,
    AdminSection::Security => IconName::Key,
    AdminSection::Audit => IconName::List,
    AdminSection::Import => IconName::Download,
    AdminSection::Brands => IconName::Spotlight,
    AdminSection::Backgrounds => IconName::Image,
    AdminSection::Emoji => IconName::Smile,
    AdminSection::Stickers => IconName::Sticker,
    AdminSection::Instance => IconName::Monitor,
    AdminSection::Settings => IconName::Sliders,
    AdminSection::Mail => IconName::Send,
    AdminSection::Calendar => IconName::Calendar,
    AdminSection::Updates => IconName::Download,
    AdminSection::Integrations => IconName::Terminal,
    AdminSection::Webhooks => IconName::Link,
    AdminSection::Notetaker => IconName::Compose,
  }
}

/// Whether every word of the query is found in one of the texts, ignoring case: "mail brand" finds
/// a page whose name says mail and whose keywords say brand.
fn matches_query(query: &str, texts: &[String]) -> bool {
  let haystack = texts.join(" ").to_lowercase();
  query
    .split_whitespace()
    .all(|word| haystack.contains(&word.to_lowercase()))
}

/// The frame every admin page starts with (issue #958): the group it belongs to, its title, what it
/// does, and the "metadata only" boundary with its explanation one click away.
#[component]
pub fn SectionHeader(
  #[prop(into)] title: Signal<String>,
  #[prop(into)] subtitle: Signal<String>,
) -> impl IntoView {
  let i18n = use_i18n();
  let maybe_group = use_context::<OpenSection>()
    .map(|OpenSection(section)| section.group())
    .filter(|group| *group != AdminGroup::Overview);
  view! {
    <header class="admin-section-head">
      <div class="admin-section-titles">
        {maybe_group.map(|group| view! { <p class="admin-eyebrow">{move || group_label(i18n, group)}</p> })}
        <h2>{move || title.get()}</h2>
        <p class="muted-text">{move || subtitle.get()}</p>
      </div>
      <details class="metadata-note">
        <summary class="metadata-pill">
          <Icon name=IconName::EyeOff />
          <span>{move || i18n.t("admin-metadata-only")}</span>
          <Icon name=IconName::Info class="metadata-more" />
        </summary>
        <p class="metadata-detail">{move || i18n.t("admin-metadata-detail")}</p>
      </details>
    </header>
  }
}

/// The foot of a list that loads page by page (issue #1241): "Show more" while the instance has
/// more entries than the list holds. The next page also loads by itself when the foot scrolls into
/// view (issue #1433); the button stays for a failed read, which the console banner reports and
/// which isn't retried on its own, and for browsers without the observer.
#[component]
pub fn ListMore(#[prop(into)] has_more: Signal<bool>, #[prop(into)] on_more: Callback<()>) -> impl IntoView {
  let i18n = use_i18n();
  let state = use_app_state();
  let is_loading = Memo::new(move |_| state.read(|backend| backend.admin_is_loading_more()));
  let has_error = Memo::new(move |_| state.read(|backend| backend.admin_error()).is_some());
  let sentinel = NodeRef::<leptos::html::Div>::new();
  let is_near_end = RwSignal::new(false);
  let observer = StoredValue::new_local(None::<NearEndObserver>);
  Effect::new(move |_| {
    let Some(element) = sentinel.get() else {
      return;
    };
    if observer.with_value(Option::is_none) {
      observer.set_value(NearEndObserver::new(&element, is_near_end));
    }
  });
  on_cleanup(move || {
    if let Some(Some(observer)) = observer.try_update_value(Option::take) {
      observer.disconnect();
    }
  });
  // Runs again when a page arrived and the foot is still in view, so a short list fills the screen.
  Effect::new(move |_| {
    if is_near_end.get() && has_more.get() && !is_loading.get() && !has_error.get() {
      on_more.run(());
    }
  });
  view! {
    <div class="admin-more-sentinel" node_ref=sentinel aria-hidden="true"></div>
    <Show when=move || has_more.get() fallback=|| ()>
      <div class="admin-more">
        <button
          type="button"
          class="btn"
          prop:disabled=move || is_loading.get()
          aria-busy=move || is_loading.get().then_some("true")
          on:click=move |_| on_more.run(())
        >
          {move || i18n.t("admin-show-more")}
        </button>
      </div>
    </Show>
  }
}

/// Follows whether the foot of a list is on screen.
#[cfg(target_arch = "wasm32")]
struct NearEndObserver {
  observer: web_sys::IntersectionObserver,
  _callback: wasm_bindgen::closure::Closure<dyn FnMut(js_sys::Array)>,
}

#[cfg(target_arch = "wasm32")]
impl NearEndObserver {
  fn new(element: &web_sys::HtmlDivElement, is_near_end: RwSignal<bool>) -> Option<Self> {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    use crate::reactive::SetIfAlive;

    let callback = Closure::<dyn FnMut(js_sys::Array)>::new(move |entries: js_sys::Array| {
      // The last entry is the newest state of the one observed element.
      if let Some(entry) = entries
        .iter()
        .last()
        .and_then(|entry| entry.dyn_into::<web_sys::IntersectionObserverEntry>().ok())
      {
        is_near_end.set_if_alive(entry.is_intersecting());
      }
    });
    let observer = web_sys::IntersectionObserver::new(callback.as_ref().unchecked_ref()).ok()?;
    observer.observe(element);
    Some(Self {
      observer,
      _callback: callback,
    })
  }

  fn disconnect(&self) {
    self.observer.disconnect();
  }
}

#[cfg(not(target_arch = "wasm32"))]
struct NearEndObserver;

#[cfg(not(target_arch = "wasm32"))]
impl NearEndObserver {
  fn new(_element: &web_sys::HtmlDivElement, _is_near_end: RwSignal<bool>) -> Option<Self> {
    None
  }

  fn disconnect(&self) {}
}

/// Says that a list ends before the instance's last entry: the console reads a bounded number of
/// pages, and an admin must not take the end of the list for the end of the instance.
#[component]
pub fn ListCutNotice(#[prop(into)] is_cut: Signal<bool>) -> impl IntoView {
  let i18n = use_i18n();
  move || {
    is_cut
      .get()
      .then(|| view! { <p class="muted-text small" role="status">{i18n.t("admin-list-cut")}</p> })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn an_empty_query_matches_every_page() {
    assert!(matches_query("", &texts(&["Mail server"])));
    assert!(matches_query("   ", &texts(&["Mail server"])));
  }

  #[test]
  fn every_word_must_be_found_in_one_of_the_texts_ignoring_case() {
    let page = texts(&["Mail server", "smtp invitations", "Server"]);
    assert!(matches_query("mail", &page));
    assert!(matches_query("SMTP mail", &page));
    assert!(!matches_query("mail calendar", &page));
  }

  fn texts(texts: &[&str]) -> Vec<String> {
    texts.iter().map(|text| (*text).to_owned()).collect()
  }
}
